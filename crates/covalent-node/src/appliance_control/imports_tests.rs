use super::*;
use crate::appliance_imports::*;
use covalent_core::appliance_import::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Host {
    context: Mutex<ImportContext>,
    reads: AtomicUsize,
    receipts: Mutex<std::collections::BTreeMap<Uuid, ImportReceipt>>,
    calls: AtomicUsize,
    effects: AtomicUsize,
    lose_after: AtomicBool,
    lose_before: AtomicBool,
}
impl Host {
    fn new() -> Self {
        Self {
            context: Mutex::new(crate::appliance_imports::tests::fixture().1),
            reads: AtomicUsize::new(0),
            receipts: Mutex::new(Default::default()),
            calls: AtomicUsize::new(0),
            effects: AtomicUsize::new(0),
            lose_after: AtomicBool::new(false),
            lose_before: AtomicBool::new(false),
        }
    }
    fn ready(&self) {
        let mut c = self.context.lock().unwrap();
        let s = &mut c.sources[0];
        s.confirmation = Some(ImportConfirmation {
            attachment_id: Uuid::new_v4(),
            preview_id: Uuid::new_v4(),
        });
        s.preview = ImportPreview {
            state: PreviewState::Ready,
            counts: Some(ImportCounts {
                eligible_files: 1,
                eligible_bytes: 5,
            }),
        };
    }
}
impl ApplianceImportHost for Host {
    fn context(&self) -> ImportHostFuture<'_, ImportContext> {
        Box::pin(async {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.context.lock().unwrap().clone())
        })
    }
    fn action<'a>(
        &'a self,
        op: ImportOperation,
        proof: &'a SignedImportApproval,
        withdrawal: Option<&'a str>,
    ) -> ImportHostFuture<'a, ImportReceipt> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut context = self.context.lock().unwrap();
            let choice =
                verify_import_approval(&context.bond, proof).map_err(|_| ImportHostError)?;
            if op == ImportOperation::Withdraw {
                verify_import_withdrawal(&context.bond, proof, withdrawal.ok_or(ImportHostError)?)
                    .map_err(|_| ImportHostError)?;
            }
            let mut records = self.receipts.lock().unwrap();
            if let Some(r) = records.get(&choice.action_id) {
                r.validate_for(proof)?;
                return Ok(r.clone());
            }
            if op == ImportOperation::Lookup {
                return Ok(crate::appliance_imports::tests::receipt(
                    proof,
                    ImportOutcome::Unknown,
                ));
            }
            if self.lose_before.swap(false, Ordering::SeqCst) {
                return Err(ImportHostError);
            }
            if !context.admits(&choice) {
                return Err(ImportHostError);
            }
            let outcome = if op == ImportOperation::Withdraw {
                ImportOutcome::Withdrawn
            } else {
                self.effects.fetch_add(1, Ordering::SeqCst);
                ImportOutcome::Accepted
            };
            let r = crate::appliance_imports::tests::receipt(proof, outcome);
            records.insert(choice.action_id, r.clone());
            context.sources[0].action_sequence += 1;
            if op == ImportOperation::Approve {
                context.sources[0].confirmation = None;
                context.sources[0].preview = ImportPreview {
                    state: PreviewState::Pending,
                    counts: None,
                };
            }
            if self.lose_after.swap(false, Ordering::SeqCst) {
                Err(ImportHostError)
            } else {
                Ok(r)
            }
        })
    }
}
fn approve_body(context: &ImportContext, mode: ImportApprovalMode) -> serde_json::Value {
    serde_json::to_value(crate::appliance_imports::tests::choice(context, mode)).unwrap()
}

#[tokio::test]
async fn legacy_wire_reader_keeps_volume_admission_on_a_modern_camera_host() {
    use crate::sync_control::{
        FolderControlOperation as Op, FolderControlPayload as Payload, send_folder_control,
    };
    let pi = Device::new();
    let phone = Device::new();
    let host = Arc::new(Host::new());
    let state = pi.state().with_appliance_imports(host.clone());
    let node = QuicNode::bind(([127, 0, 0, 1], 0).into(), pi.engine.clone(), &pi.tls).unwrap();
    let address = node.local_addr().unwrap();
    eprintln!("owned mixed-version import QUIC fixture {address}");
    let binding = pair(&phone, &pi, address);
    let store = state.controller_bond.as_ref().unwrap();
    let bond = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    {
        let mut c = host.context.lock().unwrap();
        c.bond.appliance = pi.engine.public_identity();
        c.bond.controller = phone.engine.public_identity();
        c.bond.native_generation = bond.generation;
        c.schema_version = 2;
        c.camera_service_available = Some(true);
        c.sources[0].can_remember = Some(true);
        let mut camera = c.sources[0].clone();
        camera.kind = ImportSourceKind::Camera;
        camera.source_id = "c".repeat(64);
        camera.can_remember = Some(false);
        camera.action_session = Uuid::new_v4();
        c.sources.push(camera);
    }
    let shutdown = node.shutdown_handle();
    let server = tokio::spawn(
        node.with_local_provider_enabled(false)
            .with_controller_bond(state.controller_bond.clone())
            .run(),
    );
    let read = |context_version| Op::ReadApplianceImports {
        requester_id: phone.engine.device_id(),
        target_id: pi.engine.device_id(),
        context_version,
    };
    for version in [Some(0), Some(3)] {
        assert_eq!(
            send_folder_control(phone.engine.clone(), binding.clone(), read(version))
                .await
                .unwrap(),
            Payload::Rejected
        );
    }
    assert_eq!(host.reads.load(Ordering::SeqCst), 0);
    let Payload::ApplianceImports(legacy) =
        send_folder_control(phone.engine.clone(), binding.clone(), read(None))
            .await
            .unwrap()
    else {
        panic!("legacy read unavailable")
    };
    let legacy = legacy.context.unwrap();
    assert_eq!(legacy.schema_version, 1);
    assert_eq!(legacy.sources.len(), 1);
    assert!(legacy.sources_truncated);
    assert_eq!(legacy.sources[0].kind, ImportSourceKind::Volume);
    assert!(legacy.sources[0].can_remember.is_none());
    let Payload::ApplianceImports(modern) =
        send_folder_control(phone.engine.clone(), binding.clone(), read(Some(2)))
            .await
            .unwrap()
    else {
        panic!("modern read unavailable")
    };
    let modern = modern.context.unwrap();
    assert_eq!(modern.schema_version, 2);
    assert_eq!(modern.sources.len(), 2);
    assert_eq!(modern.bond, legacy.bond);
    let choice = crate::appliance_imports::tests::choice(&legacy, ImportApprovalMode::Once);
    let bytes = choice.canonical_bytes().unwrap();
    let proof = SignedImportApproval {
        payload: String::from_utf8(bytes.clone()).unwrap(),
        signature: phone
            .engine
            .sign_transport_transcript_with_domain(choice.approval_domain(), &bytes),
    };
    let Payload::ApplianceImportReceipt(receipt) = send_folder_control(
        phone.engine.clone(),
        binding.clone(),
        Op::ApplianceImport {
            requester_id: phone.engine.device_id(),
            target_id: pi.engine.device_id(),
            operation: ImportOperation::Approve,
            proof: proof.clone(),
            withdrawal_signature: None,
        },
    )
    .await
    .unwrap() else {
        panic!("legacy proof unavailable")
    };
    assert_eq!(receipt.outcome, ImportOutcome::Accepted);
    assert!(receipt.validate_for(&proof).is_ok());
    assert_eq!(host.effects.load(Ordering::SeqCst), 1);
    store.revoke(bond.generation).unwrap();
    let reads = host.reads.load(Ordering::SeqCst);
    assert_eq!(
        send_folder_control(phone.engine.clone(), binding, read(None))
            .await
            .unwrap(),
        Payload::Rejected
    );
    assert_eq!(host.reads.load(Ordering::SeqCst), reads);
    shutdown.close();
    server.await.unwrap();
    shutdown.wait_for_release().await.unwrap();
    assert!(std::net::UdpSocket::bind(address).is_ok());
}

#[derive(Clone, Copy)]
enum ReadReply {
    LegacyDecoder,
    Refused,
    InvalidSignature,
    InvalidContext,
    Busy,
    TruncatedHeader,
    TruncatedBody,
    OversizedFrame,
}

#[tokio::test]
async fn newer_reader_retries_only_a_read_when_a_legacy_decoder_closes_the_modern_frame() {
    // Also cover the intermediate camera-capable host that predates the version field.
    for version in [1, 2] {
        negotiation_fixture(ReadReply::LegacyDecoder, version).await;
    }
}

#[tokio::test]
async fn context_negotiation_never_downgrades_a_refusal_invalid_reply_or_busy_result() {
    for reply in [
        ReadReply::Refused,
        ReadReply::InvalidSignature,
        ReadReply::InvalidContext,
        ReadReply::Busy,
        ReadReply::TruncatedHeader,
        ReadReply::TruncatedBody,
        ReadReply::OversizedFrame,
    ] {
        negotiation_fixture(reply, 1).await;
    }
}

async fn negotiation_fixture(reply: ReadReply, context_version: u16) {
    use crate::sync_control::{
        APPLIANCE_CONTROL_ALPN, FolderControlPayload as Payload, FolderControlReplay,
        FolderControlRequest, sign_response, verify_request,
    };
    // Exact strict shape of the old ReadApplianceImports variant; this is a wire fixture, not an old installed binary.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    enum LegacyRead {
        ReadApplianceImports {
            requester_id: DeviceId,
            target_id: DeviceId,
        },
    }
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
    eprintln!("owned legacy-decoder import QUIC fixture {address}");
    pair(&phone, &pi, address);
    let (_, mut context) = crate::appliance_imports::tests::fixture();
    context.bond.appliance = pi.engine.public_identity();
    context.bond.controller = phone.engine.public_identity();
    if context_version == 2 {
        context.schema_version = 2;
        context.camera_service_available = Some(true);
        context.volume_service_available = false;
        context.sources[0].kind = ImportSourceKind::Camera;
        context.sources[0].can_remember = Some(false);
    }
    if matches!(reply, ReadReply::InvalidContext) {
        context.source_cleanup_available = true;
    }
    let expected = if matches!(reply, ReadReply::LegacyDecoder) {
        2
    } else {
        1
    };
    let engine = pi.engine.clone();
    let peer = phone.engine.public_identity();
    let fingerprint = pi.tls.certificate_fingerprint();
    let listener = endpoint.clone();
    let server = tokio::spawn(async move {
        let mut replay = FolderControlReplay::default();
        for index in 0..expected {
            let connection = listener.accept().await.unwrap().await.unwrap();
            let (mut send, mut receive) = connection.accept_bi().await.unwrap();
            let bytes =
                crate::transport::read_frame(&mut receive, crate::sync_control::MAX_FRAME_BYTES)
                    .await
                    .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if matches!(reply, ReadReply::LegacyDecoder) {
                let decoded = serde_json::from_value::<LegacyRead>(value["operation"].clone());
                if index == 0 {
                    assert!(decoded.is_err());
                    connection.close(0_u32.into(), b"legacy decoder refused unknown read field");
                    continue;
                }
                let LegacyRead::ReadApplianceImports {
                    requester_id,
                    target_id,
                } = decoded.unwrap();
                assert_eq!(requester_id, peer.device_id);
                assert_eq!(target_id, engine.device_id());
            }
            let request: FolderControlRequest = serde_json::from_slice(&bytes).unwrap();
            verify_request(
                &request,
                APPLIANCE_CONTROL_ALPN,
                engine.device_id(),
                &fingerprint,
                &peer,
                &mut replay,
                request.issued_at_unix_ms,
            )
            .unwrap();
            let malformed = match reply {
                ReadReply::TruncatedHeader => Some(vec![0_u8, 0]),
                ReadReply::TruncatedBody => {
                    Some([100_u32.to_be_bytes().as_slice(), b"{}"].concat())
                }
                ReadReply::OversizedFrame => Some(
                    (crate::sync_control::MAX_FRAME_BYTES as u32 + 1)
                        .to_be_bytes()
                        .to_vec(),
                ),
                _ => None,
            };
            if let Some(bytes) = malformed {
                send.write_all(&bytes).await.unwrap();
                send.finish().unwrap();
                connection.closed().await;
                continue;
            }
            let payload = match reply {
                ReadReply::Refused => Payload::Rejected,
                ReadReply::Busy => Payload::Busy,
                _ => Payload::ApplianceImports(super::super::imports::ImportStatus {
                    schema_version: 1,
                    appliance_id: engine.device_id(),
                    controller_id: peer.device_id,
                    bond_generation: context.bond.native_generation,
                    imports_supported: true,
                    imports_available: true,
                    context: Some(context.clone()),
                }),
            };
            let mut response = sign_response(&engine, &request, &fingerprint, payload).unwrap();
            if matches!(reply, ReadReply::InvalidSignature) {
                response.signature = "A".repeat(86);
            }
            crate::transport::write_frame(&mut send, &serde_json::to_vec(&response).unwrap())
                .await
                .unwrap();
            send.finish().unwrap();
            connection.closed().await;
        }
        expected
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        api(
            phone.state(),
            "POST",
            "/api/v1/appliance/imports/context",
            serde_json::json!({"peerId":pi.engine.device_id()}),
            true,
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        result.0,
        match reply {
            ReadReply::LegacyDecoder => StatusCode::OK,
            ReadReply::Refused => StatusCode::FORBIDDEN,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        }
    );
    if matches!(reply, ReadReply::LegacyDecoder) {
        assert_eq!(result.1["context"]["schemaVersion"], context_version);
    }
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap(),
        expected
    );
    endpoint.close(0_u32.into(), b"fixture complete");
    endpoint.wait_idle().await;
    drop(endpoint);
    // Quinn's detached endpoint driver can hold the socket after wait_idle/drop.
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match std::net::UdpSocket::bind(address) {
                Ok(socket) => {
                    drop(socket);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await
                }
                Err(error) => panic!("owned QUIC fixture socket release: {error}"),
            }
        }
    })
    .await
    .expect("owned QUIC fixture socket was not released");
}

#[tokio::test]
async fn independently_reapproved_bond_can_import_without_replaying_or_resolving_an_old_request() {
    let pi = Device::new();
    let phone = Device::new();
    let host = Arc::new(Host::new());
    host.ready();
    let state = pi.state().with_appliance_imports(host.clone());
    let node = QuicNode::bind(([127, 0, 0, 1], 0).into(), Arc::clone(&pi.engine), &pi.tls).unwrap();
    let address = node.local_addr().unwrap();
    eprintln!("owned renewed-bond QUIC fixture {address}");
    pair(&phone, &pi, address);
    let store = state.controller_bond.as_ref().unwrap();
    let bond = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    {
        let mut c = host.context.lock().unwrap();
        c.bond.appliance = pi.engine.public_identity();
        c.bond.controller = phone.engine.public_identity();
        c.bond.native_generation = bond.generation;
    }
    let shutdown = node.shutdown_handle();
    let server = tokio::spawn(
        node.with_local_provider_enabled(false)
            .with_controller_bond(state.controller_bond.clone())
            .run(),
    );
    let phone_state = phone.state();
    let original = crate::appliance_imports::tests::choice(
        &host.context.lock().unwrap(),
        ImportApprovalMode::Once,
    );
    host.lose_before.store(true, Ordering::SeqCst);
    let (code, pending) = api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/imports/confirm",
        serde_json::to_value(&original).unwrap(),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert!(pending["receipt"].is_null());
    assert_eq!(host.effects.load(Ordering::SeqCst), 0);
    store.revoke(bond.generation).unwrap();
    let new = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    {
        let mut c = host.context.lock().unwrap();
        c.bond.native_generation = new.generation;
        c.bond.root_generation = Uuid::new_v4();
        c.sources[0].source_id = "b".repeat(64); // The old card is no longer attached.
    }
    let replacement = approve_body(&host.context.lock().unwrap(), ImportApprovalMode::Once);
    let (code, accepted) = api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/imports/confirm",
        replacement,
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(accepted["receipt"]["outcome"], "accepted");
    assert_eq!(host.effects.load(Ordering::SeqCst), 1);
    let proof: SignedImportApproval = serde_json::from_value(pending["proof"].clone()).unwrap();
    assert!(
        host.action(ImportOperation::Approve, &proof, None)
            .await
            .is_err()
    );
    let (code, old) = api(
        phone_state,
        "POST",
        "/api/v1/appliance/imports/check",
        serde_json::json!({"actionId":original.action_id}),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert!(old["receipt"].is_null());
    assert_eq!(host.effects.load(Ordering::SeqCst), 1);
    shutdown.close();
    server.await.unwrap();
    shutdown.wait_for_release().await.unwrap();
    assert!(std::net::UdpSocket::bind(address).is_ok());
}

#[tokio::test]
async fn actual_quic_import_confirmation_persists_before_send_and_only_reads_after_lost_reply() {
    run_import_confirmation(false).await;
}
#[tokio::test]
async fn actual_quic_import_camera_uses_new_domain_and_readback_without_repeating_work() {
    run_import_confirmation(true).await;
}
async fn run_import_confirmation(camera: bool) {
    let pi = Device::new();
    let phone = Device::new();
    let other = Device::new();
    let host = Arc::new(Host::new());
    if camera {
        let mut c = host.context.lock().unwrap();
        c.schema_version = 2;
        c.camera_service_available = Some(true);
        c.volume_service_available = false;
        c.sources[0].kind = ImportSourceKind::Camera;
        c.sources[0].can_remember = Some(false);
    }
    let state = pi.state().with_appliance_imports(host.clone());
    let node = QuicNode::bind(([127, 0, 0, 1], 0).into(), Arc::clone(&pi.engine), &pi.tls).unwrap();
    let address = node.local_addr().unwrap();
    eprintln!("owned import QUIC fixture {address}");
    pair(&phone, &pi, address);
    pair(&other, &pi, address);
    let shutdown = node.shutdown_handle();
    let server = tokio::spawn(
        node.with_local_provider_enabled(false)
            .with_controller_bond(state.controller_bond.clone())
            .run(),
    );
    let phone_state = phone.state();
    let peer = serde_json::json!({"peerId":pi.engine.device_id()});
    assert_eq!(
        api(
            phone_state.clone(),
            "POST",
            "/api/v1/appliance/imports/context",
            peer.clone(),
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
            "/api/v1/appliance/imports/context",
            peer.clone(),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    let store = state.controller_bond.as_ref().unwrap();
    let bond = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    {
        let mut c = host.context.lock().unwrap();
        c.bond.appliance = pi.engine.public_identity();
        c.bond.controller = phone.engine.public_identity();
        c.bond.native_generation = bond.generation;
    }
    let (code, value) = api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/imports/context",
        peer.clone(),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(value["importsAvailable"], true);
    let context: ImportContext = serde_json::from_value(value["context"].clone()).unwrap();
    let body = approve_body(&context, ImportApprovalMode::Once);
    if camera {
        let mut remember = body.clone();
        remember["mode"] = "remember".into();
        remember["actionId"] = Uuid::new_v4().to_string().into();
        assert_eq!(
            api(
                phone_state.clone(),
                "POST",
                "/api/v1/appliance/imports/confirm",
                remember,
                true
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(host.effects.load(Ordering::SeqCst), 0);
    }
    let id = body["actionId"].clone();
    host.lose_after.store(true, Ordering::SeqCst);
    let (code, record) = api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/imports/confirm",
        body.clone(),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert!(record["receipt"].is_null());
    assert_eq!(host.effects.load(Ordering::SeqCst), 1);
    api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/imports/confirm",
        body.clone(),
        true,
    )
    .await;
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    drop(phone_state);
    let phone_state = phone.state();
    let (code, checked) = api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/imports/check",
        serde_json::json!({"actionId":id}),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(checked["receipt"]["outcome"], "accepted");
    assert_eq!(host.effects.load(Ordering::SeqCst), 1);
    assert_eq!(
        api(
            other.state(),
            "POST",
            "/api/v1/appliance/imports/context",
            peer.clone(),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut changed = body;
    changed["mode"] = "remember".into();
    assert_eq!(
        api(
            phone_state.clone(),
            "POST",
            "/api/v1/appliance/imports/confirm",
            changed,
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    store.revoke(bond.generation).unwrap();
    assert_eq!(
        api(
            phone_state,
            "POST",
            "/api/v1/appliance/imports/context",
            peer,
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    shutdown.close();
    server.await.unwrap();
    shutdown.wait_for_release().await.unwrap();
    assert!(std::net::UdpSocket::bind(address).is_ok());
}

#[tokio::test]
async fn withdrawal_requires_a_distinct_signature_and_late_original_request_cannot_execute() {
    let pi = Device::new();
    let phone = Device::new();
    let host = Arc::new(Host::new());
    let state = pi.state().with_appliance_imports(host.clone());
    let node = QuicNode::bind(([127, 0, 0, 1], 0).into(), Arc::clone(&pi.engine), &pi.tls).unwrap();
    let address = node.local_addr().unwrap();
    eprintln!("owned withdrawal QUIC fixture {address}");
    let binding = pair(&phone, &pi, address);
    let store = state.controller_bond.as_ref().unwrap();
    let bond = store
        .grant(
            &pi.engine,
            store.snapshot().unwrap().generation,
            phone.engine.public_identity(),
        )
        .unwrap();
    {
        let mut c = host.context.lock().unwrap();
        c.bond.appliance = pi.engine.public_identity();
        c.bond.controller = phone.engine.public_identity();
        c.bond.native_generation = bond.generation;
    }
    host.ready();
    let shutdown = node.shutdown_handle();
    let server = tokio::spawn(
        node.with_local_provider_enabled(false)
            .with_controller_bond(state.controller_bond.clone())
            .run(),
    );
    let phone_state = phone.state();
    let body = approve_body(&host.context.lock().unwrap(), ImportApprovalMode::Once);
    let id = body["actionId"].clone();
    host.lose_before.store(true, Ordering::SeqCst);
    let (_, pending) = api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/imports/confirm",
        body,
        true,
    )
    .await;
    assert!(pending["receipt"].is_null());
    let proof: SignedImportApproval = serde_json::from_value(pending["proof"].clone()).unwrap();
    assert_eq!(
        store
            .import_action(
                &pi.engine,
                phone.engine.device_id(),
                ImportOperation::Withdraw,
                &proof,
                Some(&proof.signature)
            )
            .await,
        Err(crate::sync_control::FolderControlError::Rejected)
    );
    let (code, withdrawn) = api(
        phone_state.clone(),
        "POST",
        "/api/v1/appliance/imports/withdraw",
        serde_json::json!({"actionId":id}),
        true,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(withdrawn["receipt"]["outcome"], "withdrawn");
    assert_eq!(host.effects.load(Ordering::SeqCst), 0);
    let result = crate::sync_control::send_folder_control(
        Arc::clone(&phone.engine),
        binding,
        crate::sync_control::FolderControlOperation::ApplianceImport {
            requester_id: phone.engine.device_id(),
            target_id: pi.engine.device_id(),
            operation: ImportOperation::Approve,
            proof,
            withdrawal_signature: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(result,crate::sync_control::FolderControlPayload::ApplianceImportReceipt(r) if r.outcome==ImportOutcome::Withdrawn)
    );
    assert_eq!(host.effects.load(Ordering::SeqCst), 0);
    shutdown.close();
    server.await.unwrap();
    shutdown.wait_for_release().await.unwrap();
    assert!(std::net::UdpSocket::bind(address).is_ok());
}
