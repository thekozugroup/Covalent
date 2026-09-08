use super::*;
use crate::transport::TlsIdentity;
use covalent_core::{EngineOptions, KeyProtector, StaticKeyProtector};
use covalent_protocol::{PeerRole, TransportBinding};
use std::collections::BTreeSet;
use std::sync::Arc;

struct TestEngine {
    _directory: tempfile::TempDir,
    engine: Arc<Engine>,
    protector: Arc<dyn KeyProtector>,
}

fn test_engine() -> TestEngine {
    let directory = tempfile::tempdir().expect("temporary engine directory");
    let protector: Arc<dyn KeyProtector> =
        Arc::new(StaticKeyProtector::new(1, [0x5a; 32]).expect("test protector"));
    let engine = Arc::new(
        Engine::open(
            EngineOptions::new(directory.path().join("node"))
                .with_key_protector(Arc::clone(&protector)),
        )
        .expect("test engine"),
    );
    TestEngine {
        _directory: directory,
        engine,
        protector,
    }
}

impl TestEngine {
    fn tls(&self) -> TlsIdentity {
        TlsIdentity::load_or_create(
            self._directory.path().join("tls"),
            self._directory.path(),
            self.protector.as_ref(),
        )
        .expect("TLS identity")
    }

    fn transport(&self, tls: &TlsIdentity, address: std::net::SocketAddr) -> TransportBinding {
        TransportBinding {
            peer_id: self.engine.device_id(),
            display_name: self.engine.config().expect("config").device_name,
            address: address.to_string(),
            certificate_der: URL_SAFE_NO_PAD.encode(tls.certificate_der()),
            certificate_fingerprint: tls.certificate_fingerprint(),
        }
    }
}

fn operation(source: DeviceId) -> FolderControlOperation {
    FolderControlOperation::SendCommit {
        offer_id: uuid::Uuid::new_v4(),
        commit: FolderShareCommit {
            schema_version: 1,
            source_device_id: source,
            offer_digest: "a".repeat(64),
            acceptance_digest: "b".repeat(64),
            signature: "not-an-inner-record-signature".to_owned(),
        },
    }
}

fn resign(engine: &Engine, request: &mut FolderControlRequest) {
    request.signature = engine.sign_transport_transcript_with_domain(
        REQUEST_DOMAIN,
        &request_bytes(request).expect("transcript"),
    );
}

#[test]
fn signed_request_and_response_bind_identity_nonce_digest_and_certificate() {
    let source = test_engine();
    let target = test_engine();
    let fingerprint = "c".repeat(64);
    let request = sign_request(
        &source.engine,
        target.engine.device_id(),
        &fingerprint,
        operation(source.engine.device_id()),
    )
    .expect("request");
    let mut replay = FolderControlReplay::default();
    verify_request(
        &request,
        target.engine.device_id(),
        &fingerprint,
        &source.engine.public_identity(),
        &mut replay,
        request.issued_at_unix_ms,
    )
    .expect("verified request");
    let response = sign_response(
        &target.engine,
        &request,
        &fingerprint,
        FolderControlPayload::Ack,
    )
    .expect("response");
    verify_response(
        &request,
        &response,
        &target.engine.public_identity(),
        &fingerprint,
    )
    .expect("verified response");

    let mut substituted = response.clone();
    substituted.request_nonce = URL_SAFE_NO_PAD.encode([7_u8; 24]);
    assert_eq!(
        verify_response(
            &request,
            &substituted,
            &target.engine.public_identity(),
            &fingerprint,
        ),
        Err(FolderControlError::Rejected)
    );
    assert_eq!(
        verify_response(
            &request,
            &response,
            &target.engine.public_identity(),
            &"d".repeat(64),
        ),
        Err(FolderControlError::Rejected)
    );
}

#[test]
fn request_rejects_forwarding_version_time_and_signature_tampering() {
    let source = test_engine();
    let target = test_engine();
    let other = test_engine();
    let fingerprint = "c".repeat(64);
    let request = sign_request(
        &source.engine,
        target.engine.device_id(),
        &fingerprint,
        operation(source.engine.device_id()),
    )
    .expect("request");
    let verify = |request: &FolderControlRequest| {
        verify_request(
            request,
            target.engine.device_id(),
            &fingerprint,
            &source.engine.public_identity(),
            &mut FolderControlReplay::default(),
            request.issued_at_unix_ms,
        )
    };

    let mut forwarded = request.clone();
    forwarded.target_id = other.engine.device_id();
    assert_eq!(verify(&forwarded), Err(FolderControlError::Rejected));

    let mut wrong_version = request.clone();
    wrong_version.schema_version = SCHEMA_VERSION + 1;
    resign(&source.engine, &mut wrong_version);
    assert_eq!(verify(&wrong_version), Err(FolderControlError::Rejected));

    let mut stale = request.clone();
    stale.issued_at_unix_ms = stale
        .issued_at_unix_ms
        .saturating_sub(FRESHNESS.as_millis() as u64 + 1);
    resign(&source.engine, &mut stale);
    assert_eq!(
        verify_request(
            &stale,
            target.engine.device_id(),
            &fingerprint,
            &source.engine.public_identity(),
            &mut FolderControlReplay::default(),
            request.issued_at_unix_ms,
        ),
        Err(FolderControlError::Rejected)
    );

    let mut tampered = request;
    tampered.operation_digest = "0".repeat(64);
    assert_eq!(verify(&tampered), Err(FolderControlError::Rejected));

    let mut noncanonical_nonce = wrong_version;
    noncanonical_nonce.schema_version = SCHEMA_VERSION;
    noncanonical_nonce.nonce.push('=');
    resign(&source.engine, &mut noncanonical_nonce);
    assert_eq!(
        verify(&noncanonical_nonce),
        Err(FolderControlError::Rejected)
    );
}

#[test]
fn replay_window_never_evicts_a_fresh_nonce_at_capacity() {
    let peer = DeviceId::new();
    let mut replay = FolderControlReplay::default();
    for index in 0..MAX_NONCES_PER_PEER {
        replay.insert(peer, &format!("n{index}"), 100).unwrap();
    }
    assert_eq!(
        replay.insert(peer, "new", 100),
        Err(FolderControlError::Rejected)
    );
    assert_eq!(
        replay.insert(peer, "n0", 100),
        Err(FolderControlError::Rejected)
    );
}

#[test]
fn replay_window_expires_only_after_the_fixed_ttl() {
    let peer = DeviceId::new();
    let mut replay = FolderControlReplay::default();
    replay.insert(peer, "nonce", 1).unwrap();
    assert_eq!(
        replay.insert(peer, "nonce", 1 + REPLAY_RETENTION.as_millis() as u64),
        Err(FolderControlError::Rejected)
    );
    replay
        .insert(peer, "nonce", 2 + REPLAY_RETENTION.as_millis() as u64)
        .unwrap();
}

#[test]
fn replay_covers_the_entire_future_skew_freshness_window() {
    let source = test_engine();
    let target = test_engine();
    let fingerprint = "c".repeat(64);
    let mut request = sign_request(
        &source.engine,
        target.engine.device_id(),
        &fingerprint,
        operation(source.engine.device_id()),
    )
    .expect("request");
    let receipt = request.issued_at_unix_ms;
    request.issued_at_unix_ms = receipt + FRESHNESS.as_millis() as u64;
    resign(&source.engine, &mut request);
    let mut replay = FolderControlReplay::default();
    verify_request(
        &request,
        target.engine.device_id(),
        &fingerprint,
        &source.engine.public_identity(),
        &mut replay,
        receipt,
    )
    .expect("future-edge request accepted");
    // The same request is still fresh just before receipt + two windows, and
    // its nonce must therefore still be retained.
    assert_eq!(
        verify_request(
            &request,
            target.engine.device_id(),
            &fingerprint,
            &source.engine.public_identity(),
            &mut replay,
            receipt + REPLAY_RETENTION.as_millis() as u64,
        ),
        Err(FolderControlError::Rejected)
    );
}

#[tokio::test]
async fn authenticated_request_observes_busy_when_all_mutation_slots_are_held() {
    let source = test_engine();
    let target = test_engine();
    let fingerprint = "c".repeat(64);
    let request = sign_request(
        &source.engine,
        target.engine.device_id(),
        &fingerprint,
        operation(source.engine.device_id()),
    )
    .expect("request");
    let admission = FolderControlAdmission::default();
    verify_request(
        &request,
        target.engine.device_id(),
        &fingerprint,
        &source.engine.public_identity(),
        &mut admission.replay.lock().expect("replay lock"),
        request.issued_at_unix_ms,
    )
    .expect("authenticated request");
    let mut held = Vec::new();
    for _ in 0..MAX_FOLDER_STREAMS {
        held.push(
            admission
                .streams
                .clone()
                .try_acquire_owned()
                .expect("mutation slot"),
        );
    }
    assert!(admission.streams.clone().try_acquire_owned().is_err());
    drop(held);
}

#[test]
fn frame_limit_rejects_oversized_serialized_data() {
    assert_eq!(
        enforce_frame(&"x".repeat(MAX_FRAME_BYTES)),
        Err(FolderControlError::Invalid)
    );
}

#[tokio::test]
async fn pinned_v4_client_uses_one_real_loopback_quic_stream() {
    let source = test_engine();
    let target = test_engine();
    let source_tls = source.tls();
    let target_tls = target.tls();
    let endpoint = quinn::Endpoint::server(
        target_tls
            .server_config_with_alpns(&[FOLDER_CONTROL_ALPN])
            .expect("v4 server configuration"),
        ([127, 0, 0, 1], 0).into(),
    )
    .expect("loopback endpoint");
    let address = endpoint.local_addr().expect("loopback address");
    let target_binding = target.transport(&target_tls, address);
    let source_binding = source.transport(&source_tls, ([127, 0, 0, 1], 43123).into());

    let invitation = source
        .engine
        .pairing_manager()
        .create_invitation_with_transport(
            1_000,
            60_000,
            vec![source_binding.address.clone()],
            source_binding,
        )
        .expect("invitation");
    let roles = BTreeSet::from([PeerRole::BackupReader]);
    let mut session = target
        .engine
        .accept_pairing_with_transport(
            invitation,
            target_binding.clone(),
            roles.clone(),
            roles,
            1_001,
        )
        .expect("accept pairing");
    let code = session.authentication_string().as_str().to_owned();
    target
        .engine
        .confirm_pairing_as_responder(&mut session, &code, 1_002)
        .expect("responder confirmation");
    source
        .engine
        .confirm_pairing_as_inviter(&mut session, &code, 1_003)
        .expect("inviter confirmation");
    source
        .engine
        .finalize_pairing_as_inviter(&session, 1_004)
        .expect("retained target pin");

    let server_engine = Arc::clone(&target.engine);
    let server_fingerprint = target_binding.certificate_fingerprint.clone();
    let server = tokio::spawn(async move {
        let connecting = endpoint.accept().await.expect("incoming v4 connection");
        let connection = connecting.await.expect("v4 handshake");
        let (mut send, mut receive) = connection.accept_bi().await.expect("one stream");
        let request: FolderControlRequest = serde_json::from_slice(
            &crate::transport::read_frame(&mut receive, MAX_FRAME_BYTES)
                .await
                .expect("request frame"),
        )
        .expect("request schema");
        let response = sign_response(
            &server_engine,
            &request,
            &server_fingerprint,
            FolderControlPayload::Ack,
        )
        .expect("response");
        crate::transport::write_frame(
            &mut send,
            &serde_json::to_vec(&response).expect("response JSON"),
        )
        .await
        .expect("response frame");
        send.finish().expect("finish response");
        let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
        connection.close(0_u32.into(), b"test complete");
        endpoint.close(0_u32.into(), b"test complete");
        endpoint.wait_idle().await;
    });

    assert_eq!(
        send_folder_control(
            Arc::clone(&source.engine),
            target_binding,
            operation(source.engine.device_id()),
        )
        .await,
        Ok(FolderControlPayload::Ack)
    );
    server.await.expect("server task");
}
