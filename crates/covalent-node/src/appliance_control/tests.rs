use super::*;
use crate::transport::{QuicNode, TlsIdentity};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{EngineOptions, KeyProtector, StaticKeyProtector};
use covalent_protocol::{PeerRole, TransportBinding};
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

#[path = "imports_tests.rs"]
mod imports_tests;

struct Device {
    root: tempfile::TempDir,
    engine: Arc<Engine>,
    protector: Arc<dyn KeyProtector>,
    tls: TlsIdentity,
}

impl Device {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let protector: Arc<dyn KeyProtector> =
            Arc::new(StaticKeyProtector::new(1, [67; 32]).unwrap());
        let engine = Arc::new(
            Engine::open(
                EngineOptions::new(root.path().join("node"))
                    .with_key_protector(Arc::clone(&protector)),
            )
            .unwrap(),
        );
        let tls =
            TlsIdentity::load_or_create(root.path().join("tls"), root.path(), protector.as_ref())
                .unwrap();
        Self {
            root,
            engine,
            protector,
            tls,
        }
    }
    fn bond_path(&self) -> PathBuf {
        self.engine
            .store()
            .root()
            .parent()
            .unwrap()
            .join("controller-bond.json")
    }
    fn store(&self) -> ControllerBondStore {
        ControllerBondStore::open(self.bond_path(), self.engine.public_identity()).unwrap()
    }
    fn binding(&self, address: std::net::SocketAddr) -> TransportBinding {
        TransportBinding {
            peer_id: self.engine.device_id(),
            display_name: self.engine.config().unwrap().device_name,
            address: address.to_string(),
            certificate_der: URL_SAFE_NO_PAD.encode(self.tls.certificate_der()),
            certificate_fingerprint: self.tls.certificate_fingerprint(),
        }
    }
    fn state(&self) -> AppState {
        AppState::new(
            Arc::clone(&self.engine),
            covalent_protocol::PlatformTier::Tier1,
            "t".repeat(48),
        )
        .unwrap()
    }
    fn service(&self, address: std::net::SocketAddr) -> Arc<crate::sync_engine::FolderSyncService> {
        use crate::sync_engine::{
            EngineInstallation, FolderSharingJournal, FolderSyncService, VerifiedEngineExecutable,
        };
        let path = self.root.path().join("sync");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let installation =
            Arc::new(EngineInstallation::create(&path, self.protector.as_ref()).unwrap());
        let journal = FolderSharingJournal::create(
            Arc::clone(&self.engine),
            Arc::clone(&installation),
            Arc::clone(&self.protector),
            address,
            address,
        )
        .unwrap();
        // No folders or service.start(): this inert fixture must never execute.
        let worker = self.root.path().join("inert-worker");
        let bytes = b"#!/bin/sh\nexit 99\n";
        std::fs::write(&worker, bytes).unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
        use sha2::Digest as _;
        let digest: [u8; 32] = sha2::Sha256::digest(bytes).into();
        Arc::new(
            FolderSyncService::new(
                journal,
                installation,
                Arc::clone(&self.engine),
                VerifiedEngineExecutable::open(&worker, digest).unwrap(),
                VerifiedEngineExecutable::open(&worker, digest).unwrap(),
                self.root.path().join("runtime"),
            )
            .unwrap(),
        )
    }
}

fn pair(source: &Device, target: &Device, address: std::net::SocketAddr) -> TransportBinding {
    let binding = target.binding(address);
    let source_binding = source.binding(([127, 0, 0, 1], 43123).into());
    let invitation = source
        .engine
        .pairing_manager()
        .create_invitation_with_transport(
            1000,
            60000,
            vec![source_binding.address.clone()],
            source_binding,
        )
        .unwrap();
    let roles = BTreeSet::from([PeerRole::BackupReader]);
    let mut session = target
        .engine
        .accept_pairing_with_transport(invitation, binding.clone(), roles.clone(), roles, 1001)
        .unwrap();
    let code = session.authentication_string().as_str().to_owned();
    target
        .engine
        .confirm_pairing_as_responder(&mut session, &code, 1002)
        .unwrap();
    source
        .engine
        .confirm_pairing_as_inviter(&mut session, &code, 1003)
        .unwrap();
    source
        .engine
        .finalize_pairing_as_inviter(&session, 1004)
        .unwrap();
    target
        .engine
        .finalize_pairing_as_responder(&session, 1004)
        .unwrap();
    binding
}

#[test]
fn pairing_does_not_grant_control_and_bonds_pin_the_full_key() {
    let pi = Device::new();
    let phone = Device::new();
    let other = Device::new();
    pair(&phone, &pi, ([127, 0, 0, 1], 43124).into());
    pair(&other, &pi, ([127, 0, 0, 1], 43124).into());
    let store = pi.store();
    let empty = store.snapshot().unwrap();
    assert!(
        store
            .authorized(&pi.engine, phone.engine.device_id())
            .is_err()
    );
    let forged = PublicIdentity::from_encoded(
        phone.engine.device_id(),
        other.engine.public_identity().public_key,
    )
    .unwrap();
    assert_eq!(
        store.grant(&pi.engine, empty.generation, forged),
        Err(BondError::Rejected)
    );
    let bond = store
        .grant(&pi.engine, empty.generation, phone.engine.public_identity())
        .unwrap();
    assert_eq!(
        store
            .grant(&pi.engine, empty.generation, phone.engine.public_identity())
            .unwrap(),
        bond
    );
    assert_eq!(
        store
            .authorized(&pi.engine, phone.engine.device_id())
            .unwrap(),
        bond
    );
    assert!(
        store
            .authorized(&pi.engine, other.engine.device_id())
            .is_err()
    );
    assert_eq!(
        store.grant(&pi.engine, bond.generation, other.engine.public_identity()),
        Err(BondError::Conflict)
    );
    assert_eq!(
        store
            .grant(&pi.engine, bond.generation, phone.engine.public_identity())
            .unwrap(),
        bond
    );
    assert!(ControllerBondStore::open(pi.bond_path(), pi.engine.public_identity()).is_none());
    drop(store);
    let reopened = pi.store();
    assert_eq!(reopened.snapshot().unwrap(), bond);
    assert_eq!(
        std::fs::metadata(pi.bond_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let another_pi = Device::new();
    drop(reopened);
    assert!(
        ControllerBondStore::open(pi.bond_path(), another_pi.engine.public_identity()).is_none()
    );
}

#[test]
fn revocation_is_durable_and_stale_confirmations_cannot_replace_the_owner() {
    let pi = Device::new();
    let phone = Device::new();
    let next = Device::new();
    pair(&phone, &pi, ([127, 0, 0, 1], 43124).into());
    pair(&next, &pi, ([127, 0, 0, 1], 43124).into());
    let store = pi.store();
    let old = store.snapshot().unwrap();
    let bond = store
        .grant(&pi.engine, old.generation, phone.engine.public_identity())
        .unwrap();
    assert_eq!(store.revoke(old.generation), Err(BondError::Conflict));
    let revoked = store.revoke(bond.generation).unwrap();
    drop(store);
    let store = pi.store();
    assert!(
        store
            .authorized(&pi.engine, phone.engine.device_id())
            .is_err()
    );
    assert_eq!(
        store.grant(&pi.engine, bond.generation, phone.engine.public_identity()),
        Err(BondError::Conflict)
    );
    let changed = store
        .grant(
            &pi.engine,
            revoked.generation,
            next.engine.public_identity(),
        )
        .unwrap();
    assert_ne!(changed.generation, bond.generation);
    assert!(
        store
            .authorized(&pi.engine, phone.engine.device_id())
            .is_err()
    );
    pi.engine.revoke_peer(next.engine.device_id()).unwrap();
    assert!(
        store
            .authorized(&pi.engine, next.engine.device_id())
            .is_err()
    );
}

#[test]
fn malformed_alias_and_special_authority_files_fail_closed_without_replacement() {
    let pi = Device::new();
    let path = pi.bond_path();
    std::fs::write(&path, b"invalid retained state").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(ControllerBondStore::open(path.clone(), pi.engine.public_identity()).is_none());
    assert!(pi.state().controller_bond.is_none());
    assert_eq!(std::fs::read(&path).unwrap(), b"invalid retained state");
    std::fs::remove_file(&path).unwrap();
    let record = pi.store().snapshot().unwrap();
    let mut missing = serde_json::to_value(&record).unwrap();
    missing.as_object_mut().unwrap().remove("controller");
    let mut wrong_version = serde_json::to_value(&record).unwrap();
    wrong_version["schemaVersion"] = serde_json::json!(2);
    let mut nil_generation = serde_json::to_value(&record).unwrap();
    nil_generation["generation"] = serde_json::json!(Uuid::nil());
    for invalid in [missing, wrong_version, nil_generation] {
        let bytes = serde_json::to_vec(&invalid).unwrap();
        crate::persist_private_file(&path, &bytes).unwrap();
        assert!(ControllerBondStore::open(path.clone(), pi.engine.public_identity()).is_none());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    crate::persist_private_file(&path, &serde_json::to_vec(&record).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(ControllerBondStore::open(path.clone(), pi.engine.public_identity()).is_none());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let alias = path.with_extension("alias");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(ControllerBondStore::open(path.clone(), pi.engine.public_identity()).is_none());
    std::fs::remove_file(&alias).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&alias, &path).unwrap();
    assert!(ControllerBondStore::open(path.clone(), pi.engine.public_identity()).is_none());
    std::fs::remove_file(&path).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    assert!(ControllerBondStore::open(path, pi.engine.public_identity()).is_none());
}

#[test]
fn failed_state_write_disables_control_instead_of_retaining_live_authority() {
    let pi = Device::new();
    let phone = Device::new();
    pair(&phone, &pi, ([127, 0, 0, 1], 43124).into());
    let store = pi.store();
    let empty = store.snapshot().unwrap();
    let bond = store
        .grant(&pi.engine, empty.generation, phone.engine.public_identity())
        .unwrap();
    let retained = pi.bond_path().with_extension("retained");
    std::fs::rename(pi.bond_path(), &retained).unwrap();
    std::fs::create_dir(pi.bond_path()).unwrap();
    assert_eq!(store.revoke(bond.generation), Err(BondError::Unavailable));
    assert_eq!(
        store.authorized(&pi.engine, phone.engine.device_id()),
        Err(BondError::Unavailable)
    );
    assert_eq!(
        serde_json::from_slice::<ControllerBond>(&std::fs::read(retained).unwrap()).unwrap(),
        bond
    );
}

#[test]
fn appliance_operations_bind_protocol_target_signer_and_replay_nonce() {
    use crate::sync_control::*;
    let phone = Device::new();
    let pi = Device::new();
    let pin = "a".repeat(64);
    let operation = FolderControlOperation::ReadApplianceStatus {
        requester_id: phone.engine.device_id(),
        target_id: pi.engine.device_id(),
    };
    let request = sign_request(&phone.engine, pi.engine.device_id(), &pin, operation).unwrap();
    let mut replay = FolderControlReplay::default();
    for alpn in [FOLDER_CONTROL_ALPN, LINK_CONTROL_ALPN] {
        assert_eq!(
            verify_request(
                &request,
                alpn,
                pi.engine.device_id(),
                &pin,
                &phone.engine.public_identity(),
                &mut replay,
                request.issued_at_unix_ms
            ),
            Err(FolderControlError::Rejected)
        );
    }
    verify_request(
        &request,
        APPLIANCE_CONTROL_ALPN,
        pi.engine.device_id(),
        &pin,
        &phone.engine.public_identity(),
        &mut replay,
        request.issued_at_unix_ms,
    )
    .unwrap();
    assert_eq!(
        verify_request(
            &request,
            APPLIANCE_CONTROL_ALPN,
            pi.engine.device_id(),
            &pin,
            &phone.engine.public_identity(),
            &mut replay,
            request.issued_at_unix_ms
        ),
        Err(FolderControlError::Rejected)
    );
    for operation in [
        FolderControlOperation::ReadApplianceStatus {
            requester_id: pi.engine.device_id(),
            target_id: pi.engine.device_id(),
        },
        FolderControlOperation::ReadApplianceStatus {
            requester_id: phone.engine.device_id(),
            target_id: phone.engine.device_id(),
        },
    ] {
        assert_eq!(
            sign_request(&phone.engine, pi.engine.device_id(), &pin, operation),
            Err(FolderControlError::Rejected)
        );
    }
}

struct PlatformFixture(std::sync::atomic::AtomicUsize);
impl crate::appliance_platform::AppliancePlatformReader for PlatformFixture {
    fn read(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        crate::appliance_platform::PlatformSnapshot,
                        crate::appliance_platform::PlatformReadError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            crate::appliance_platform::PlatformSnapshot::decode(
                &serde_json::to_vec(&crate::appliance_platform::tests::fixture()).unwrap(),
            )
        })
    }
}

#[tokio::test]
async fn missing_failed_or_invalid_platform_data_is_explicitly_unavailable_without_losing_the_bond()
{
    struct Broken(bool);
    impl crate::appliance_platform::AppliancePlatformReader for Broken {
        fn read(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            crate::appliance_platform::PlatformSnapshot,
                            crate::appliance_platform::PlatformReadError,
                        >,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(async {
                if self.0 {
                    let mut value = crate::appliance_platform::tests::fixture();
                    value["sourceCleanupAvailable"] = serde_json::json!(true);
                    Ok(serde_json::from_value(value).unwrap())
                } else {
                    Err(crate::appliance_platform::PlatformReadError)
                }
            })
        }
    }
    for mode in [None, Some(false), Some(true)] {
        let pi = Device::new();
        let phone = Device::new();
        pair(&phone, &pi, ([127, 0, 0, 1], 43123).into());
        let store = pi.store();
        if let Some(invalid) = mode {
            assert!(store.platform.set(Arc::new(Broken(invalid))).is_ok());
        }
        let bond = store
            .grant(
                &pi.engine,
                store.snapshot().unwrap().generation,
                phone.engine.public_identity(),
            )
            .unwrap();
        let value = store
            .read_platform(&pi.engine, phone.engine.device_id())
            .await
            .unwrap();
        assert_eq!(value.platform_supported, mode.is_some());
        assert!(!value.platform_available);
        assert!(value.snapshot.is_none());
        assert_eq!(store.snapshot().unwrap(), bond);
    }
}

#[tokio::test]
async fn actual_quic_platform_read_requires_bond_and_never_uses_an_unapproved_host_bridge() {
    let pi = Device::new();
    let phone = Device::new();
    let other = Device::new();
    let reader = Arc::new(PlatformFixture(std::sync::atomic::AtomicUsize::new(0)));
    let state = pi.state().with_appliance_platform(reader.clone());
    let node = QuicNode::bind(([127, 0, 0, 1], 0).into(), Arc::clone(&pi.engine), &pi.tls).unwrap();
    let address = node.local_addr().unwrap();
    pair(&phone, &pi, address);
    pair(&other, &pi, address);
    let node = node
        .with_local_provider_enabled(false)
        .with_controller_bond(state.controller_bond.clone());
    let shutdown = node.shutdown_handle();
    let server = tokio::spawn(node.run());
    let phone_state = phone.state();
    let remote = serde_json::json!({"peerId":pi.engine.device_id()});
    assert_eq!(
        api(
            phone_state.clone(),
            "POST",
            "/api/v1/appliance/platform",
            remote.clone(),
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api(
            phone_state.clone(),
            "POST",
            "/api/v1/appliance/platform",
            remote.clone(),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(reader.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    let store = state.controller_bond.as_ref().unwrap();
    let bond = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    let (code, value) = api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/platform",
        remote.clone(),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(value["platformAvailable"], true);
    assert_eq!(value["bondGeneration"], serde_json::json!(bond.generation));
    assert_eq!(
        value["snapshot"]["sources"][0]["preview"]["counts"]["eligibleFiles"],
        7
    );
    assert_eq!(reader.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        api(
            other.state(),
            "POST",
            "/api/v1/appliance/platform",
            remote.clone(),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    store.revoke(bond.generation).unwrap();
    assert_eq!(
        api(
            phone_state,
            "POST",
            "/api/v1/appliance/platform",
            remote,
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(reader.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    shutdown.close();
    server.await.unwrap();
    shutdown.wait_for_release().await.unwrap();
    assert!(
        std::net::UdpSocket::bind(address).is_ok(),
        "Owned platform test socket still registered"
    );
}

#[tokio::test]
async fn platform_read_rechecks_revocation_and_does_not_queue_overlapping_work() {
    struct Blocked {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    impl crate::appliance_platform::AppliancePlatformReader for Blocked {
        fn read(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            crate::appliance_platform::PlatformSnapshot,
                            crate::appliance_platform::PlatformReadError,
                        >,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(async {
                self.entered.notify_one();
                self.release.notified().await;
                Err(crate::appliance_platform::PlatformReadError)
            })
        }
    }
    let pi = Device::new();
    let phone = Device::new();
    pair(&phone, &pi, ([127, 0, 0, 1], 43123).into());
    let store = Arc::new(pi.store());
    let reader = Arc::new(Blocked {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    assert!(store.platform.set(reader.clone()).is_ok());
    let bond = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    let task = {
        let store = store.clone();
        let engine = pi.engine.clone();
        let id = phone.engine.device_id();
        tokio::spawn(async move { store.read_platform(&engine, id).await })
    };
    reader.entered.notified().await;
    assert_eq!(
        store
            .read_platform(&pi.engine, phone.engine.device_id())
            .await,
        Err(crate::sync_control::FolderControlError::Busy)
    );
    store.revoke(bond.generation).unwrap();
    reader.release.notify_one();
    assert_eq!(
        task.await.unwrap(),
        Err(crate::sync_control::FolderControlError::Rejected)
    );
}

async fn api(
    state: AppState,
    method: &str,
    path: &str,
    body: serde_json::Value,
    authenticated: bool,
) -> (StatusCode, serde_json::Value) {
    use axum::body::Body;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if authenticated {
        request = request.header("authorization", format!("Bearer {}", "t".repeat(48)));
    }
    let response = crate::router(state)
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let code = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (code, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn actual_quic_dispatch_requires_the_owner_bond_and_honors_revocation() {
    let pi = Device::new();
    let phone = Device::new();
    let other = Device::new();
    let pi_state = pi.state();
    let phone_state = phone.state();
    let other_state = other.state();
    let node = QuicNode::bind(([127, 0, 0, 1], 0).into(), Arc::clone(&pi.engine), &pi.tls).unwrap();
    let address = node.local_addr().unwrap();
    println!("Owned appliance-test UDP endpoint: {address}");
    let service = pi.service(address);
    pair(&phone, &pi, address);
    pair(&other, &pi, address);
    let node = node
        .with_local_provider_enabled(false)
        .with_folder_service(Arc::clone(&service))
        .with_controller_bond(pi_state.controller_bond.clone());
    let shutdown = node.shutdown_handle();
    let server = tokio::spawn(node.run());
    let remote = serde_json::json!({"peerId": pi.engine.device_id()});
    assert_eq!(
        api(
            phone_state.clone(),
            "POST",
            "/api/v1/appliance/status",
            remote.clone(),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (code, snapshot) = api(
        pi_state.clone(),
        "GET",
        "/api/v1/appliance/controller",
        serde_json::Value::Null,
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(snapshot["pairedDevices"].as_array().unwrap().len(), 2);
    let grant = serde_json::json!({"generation": snapshot["bond"]["generation"], "controller": phone.engine.public_identity()});
    assert_eq!(
        api(
            pi_state.clone(),
            "POST",
            "/api/v1/appliance/controller",
            grant.clone(),
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (code, bond) = api(
        pi_state.clone(),
        "POST",
        "/api/v1/appliance/controller",
        grant,
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let (code, status) = api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/status",
        remote.clone(),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(
        status["controllerId"],
        serde_json::json!(phone.engine.device_id())
    );
    assert_eq!(
        status["applianceId"],
        serde_json::json!(pi.engine.device_id())
    );
    assert_eq!(status["bondGeneration"], bond["generation"]);
    assert_eq!(status["syncLifecycle"], "stopped");
    assert_eq!(status["syncAvailable"], true);
    assert_eq!(status["shareRecords"], 0);
    assert_eq!(status["cloudBackupVerificationAvailable"], false);
    assert_eq!(status["sourceCleanupAvailable"], false);
    assert_eq!(
        api(
            other_state,
            "POST",
            "/api/v1/appliance/status",
            remote.clone(),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        api(
            pi_state.clone(),
            "DELETE",
            "/api/v1/appliance/controller",
            serde_json::json!({"generation": snapshot["bond"]["generation"]}),
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        api(
            pi_state,
            "DELETE",
            "/api/v1/appliance/controller",
            serde_json::json!({"generation": bond["generation"]}),
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        api(
            phone_state,
            "POST",
            "/api/v1/appliance/status",
            remote,
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    shutdown.close();
    server.await.unwrap();
    shutdown.wait_for_release().await.unwrap();
    service.stop().await.unwrap();
    assert!(
        std::net::UdpSocket::bind(address).is_ok(),
        "Owned test socket still registered"
    );
}

#[tokio::test]
async fn local_api_rejects_unauthorized_oversized_and_unknown_commands() {
    let pi = Device::new();
    let state = pi.state();
    for method in ["GET", "POST", "DELETE"] {
        let payload = if method == "POST" {
            serde_json::json!({"generation": Uuid::new_v4(), "controller": pi.engine.public_identity()})
        } else {
            serde_json::json!({"generation": Uuid::new_v4()})
        };
        assert_eq!(
            api(
                state.clone(),
                method,
                "/api/v1/appliance/controller",
                payload,
                false
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        api(
            state.clone(),
            "POST",
            "/api/v1/appliance/status",
            serde_json::json!({"peerId": DeviceId::new(), "command": "delete"}),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        api(
            state.clone(),
            "POST",
            "/api/v1/appliance/controller",
            serde_json::json!({"padding": "x".repeat(5000)}),
            true
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        api(
            state,
            "POST",
            "/api/v1/appliance/status",
            serde_json::json!({"peerId": DeviceId::new()}),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn authenticated_peer_cannot_substitute_status_identity_or_cleanup_capability() {
    use crate::sync_control::{
        APPLIANCE_CONTROL_ALPN, FolderControlPayload, FolderControlRequest, MAX_FRAME_BYTES,
        sign_response,
    };
    let pi = Device::new();
    let phone = Device::new();
    let endpoint = quinn::Endpoint::server(
        pi.tls
            .server_config_with_alpns(&[APPLIANCE_CONTROL_ALPN])
            .unwrap(),
        ([127, 0, 0, 1], 0).into(),
    )
    .unwrap();
    let address = endpoint.local_addr().unwrap();
    println!("Owned adversarial appliance-test UDP endpoint: {address}");
    pair(&phone, &pi, address);
    let state = phone.state();
    let valid = ApplianceStatus {
        schema_version: 1,
        appliance_id: pi.engine.device_id(),
        controller_id: phone.engine.device_id(),
        bond_generation: Uuid::new_v4(),
        sync_available: true,
        sync_lifecycle: ApplianceSyncLifecycle::Stopped,
        share_records: Some(0),
        folder_health_fresh: false,
        cloud_backup_verification_available: false,
        source_cleanup_available: false,
    };
    let mut missing_count = serde_json::to_value(&valid).unwrap();
    missing_count
        .as_object_mut()
        .unwrap()
        .remove("shareRecords");
    assert!(serde_json::from_value::<ApplianceStatus>(missing_count).is_err());
    let mut payloads = vec![valid; 10];
    payloads[0].appliance_id = DeviceId::new();
    payloads[1].controller_id = DeviceId::new();
    payloads[2].schema_version = 2;
    payloads[3].bond_generation = Uuid::nil();
    payloads[4].cloud_backup_verification_available = true;
    payloads[5].source_cleanup_available = true;
    payloads[6].sync_available = false;
    payloads[7].sync_lifecycle = ApplianceSyncLifecycle::Unavailable;
    payloads[8].share_records = None;
    payloads[9].sync_available = false;
    payloads[9].sync_lifecycle = ApplianceSyncLifecycle::Unavailable;
    payloads[9].share_records = None;
    payloads[9].folder_health_fresh = true;
    let fingerprint = pi.tls.certificate_fingerprint();
    let engine = Arc::clone(&pi.engine);
    let server = tokio::spawn(async move {
        for payload in payloads {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut receive) = connection.accept_bi().await.unwrap();
            let request: FolderControlRequest = serde_json::from_slice(
                &crate::transport::read_frame(&mut receive, MAX_FRAME_BYTES)
                    .await
                    .unwrap(),
            )
            .unwrap();
            let response = sign_response(
                &engine,
                &request,
                &fingerprint,
                FolderControlPayload::ApplianceStatus(payload),
            )
            .unwrap();
            crate::transport::write_frame(&mut send, &serde_json::to_vec(&response).unwrap())
                .await
                .unwrap();
            send.finish().unwrap();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send.stopped()).await;
            connection.close(0_u32.into(), b"test complete");
        }
        endpoint.close(0_u32.into(), b"test complete");
        endpoint.wait_idle().await;
    });
    for _ in 0..10 {
        assert_eq!(
            api(
                state.clone(),
                "POST",
                "/api/v1/appliance/status",
                serde_json::json!({"peerId": pi.engine.device_id()}),
                true
            )
            .await
            .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    server.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if std::net::UdpSocket::bind(address).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Owned adversarial test socket still registered");
}

#[tokio::test]
async fn owner_control_stays_available_without_any_folder_sync_service() {
    let pi = Device::new();
    let phone = Device::new();
    let other = Device::new();
    let state = pi.state();
    let phone_state = phone.state();
    let other_state = other.state();
    let node = QuicNode::bind(([127, 0, 0, 1], 0).into(), Arc::clone(&pi.engine), &pi.tls).unwrap();
    let address = node.local_addr().unwrap();
    println!("Owned recovery appliance-test UDP endpoint: {address}");
    pair(&phone, &pi, address);
    pair(&other, &pi, address);
    let store = state.controller_bond.as_ref().unwrap();
    let generation = store.snapshot().unwrap().generation;
    store
        .grant(&pi.engine, generation, phone.engine.public_identity())
        .unwrap();
    // Deliberately attach no folder service, as in a failed/unconfigured sync boot.
    let node = node
        .with_local_provider_enabled(false)
        .with_controller_bond(state.controller_bond.clone());
    let shutdown = node.shutdown_handle();
    let server = tokio::spawn(node.run());
    let request = serde_json::json!({"peerId": pi.engine.device_id()});
    let (code, status) = api(
        phone_state,
        "POST",
        "/api/v1/appliance/status",
        request.clone(),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(status["syncAvailable"], false);
    assert_eq!(status["syncLifecycle"], "unavailable");
    assert!(status["shareRecords"].is_null());
    assert_eq!(status["folderHealthFresh"], false);
    assert_eq!(status["sourceCleanupAvailable"], false);
    assert_eq!(
        api(
            other_state,
            "POST",
            "/api/v1/appliance/status",
            request,
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    shutdown.close();
    server.await.unwrap();
    shutdown.wait_for_release().await.unwrap();
    assert!(
        std::net::UdpSocket::bind(address).is_ok(),
        "Owned recovery test socket still registered"
    );
}
