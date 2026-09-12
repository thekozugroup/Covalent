//! Runtime recovery handoff and cancellation regressions.
//!
//! These tests deliberately use real loopback owner/provider pairing. They
//! then replace the stopped provider socket with a UDP black hole, so the
//! cancellation assertions begin only after the recovery worker has emitted a
//! real provider request whose ordinary timeout is longer than this test's
//! shutdown deadline.

use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{Engine, EngineOptions, RecoveryUnlockKey, StaticKeyProtector};
use covalent_node::runtime::{NodeRuntime, NodeRuntimeConfig, RecoveryBootstrap};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UdpSocket;
use zeroize::Zeroizing;

const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(3);

fn loopback_zero() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

fn protector(byte: u8) -> Arc<StaticKeyProtector> {
    Arc::new(StaticKeyProtector::new(1, [byte; 32]).expect("test protector"))
}

async fn start_node(path: &Path, name: &str, key: u8) -> NodeRuntime {
    let mut configuration = NodeRuntimeConfig::new(path, loopback_zero(), loopback_zero());
    configuration.device_name = name.to_owned();
    configuration.key_protector = Some(protector(key));
    configuration.advertised_peer_address = Some(loopback_zero());
    NodeRuntime::start(configuration)
        .await
        .expect("start live node runtime")
}

fn runtime_configuration(path: &Path, key: u8) -> NodeRuntimeConfig {
    let mut configuration = NodeRuntimeConfig::new(path, loopback_zero(), loopback_zero());
    configuration.key_protector = Some(protector(key));
    configuration
}

struct HttpResponse {
    status: u16,
    body: String,
}

impl HttpResponse {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("decode JSON body {:?}: {error}", self.body))
    }
}

async fn call_at(
    address: SocketAddr,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<HttpResponse> {
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n"
    );
    match body {
        Some(body) => request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )),
        None => request.push_str("Content-Length: 0\r\n\r\n"),
    }
    let mut stream = tokio::net::TcpStream::connect(address).await?;
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let raw = String::from_utf8_lossy(&raw);
    let (head, body) = raw.split_once("\r\n\r\n").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed HTTP response")
    })?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing status"))?;
    Ok(HttpResponse {
        status,
        body: body.to_owned(),
    })
}

async fn call(node: &NodeRuntime, method: &str, path: &str, body: Option<&str>) -> HttpResponse {
    call_at(
        node.ready_info().api_address(),
        node.ready_info().api_token().expose(),
        method,
        path,
        body,
    )
    .await
    .expect("local API response")
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key} in {value:?}"))
}

async fn pair_owner_with_provider(owner: &NodeRuntime, provider: &NodeRuntime) -> Value {
    let endpoint = owner.ready_info().peer_address();
    let invitation = call(
        owner,
        "POST",
        "/api/v1/pair/invitations",
        Some(&format!(
            r#"{{"lifetimeMs":600000,"endpoints":["{endpoint}"]}}"#
        )),
    )
    .await;
    assert_eq!(invitation.status, 200, "{}", invitation.body);
    let accepted = call(
        provider,
        "POST",
        "/api/v1/pair/accept",
        Some(&format!(
            r#"{{"invitation":{},"responderName":"Recovery provider","responderRoles":["storage_provider","backup_reader"],"inviterRoles":["backup_writer","backup_reader"]}}"#,
            invitation.body
        )),
    )
    .await;
    assert_eq!(accepted.status, 200, "{}", accepted.body);
    let code = field(&accepted.json(), "authenticationString").to_owned();
    let mut session = accepted.body;
    for (node, path) in [
        (provider, "/api/v1/pair/confirm/responder"),
        (owner, "/api/v1/pair/confirm/inviter"),
    ] {
        let confirmed = call(
            node,
            "POST",
            path,
            Some(&format!(
                r#"{{"session":{session},"displayedCode":"{code}"}}"#
            )),
        )
        .await;
        assert_eq!(confirmed.status, 200, "{}", confirmed.body);
        session = confirmed.body;
    }
    let finalized = call(
        owner,
        "POST",
        "/api/v1/pair/finalize/inviter",
        Some(&format!(r#"{{"session":{session}}}"#)),
    )
    .await;
    assert_eq!(finalized.status, 200, "{}", finalized.body);
    let responder_finalized = call(
        provider,
        "POST",
        "/api/v1/pair/finalize/responder",
        Some(&format!(r#"{{"session":{session}}}"#)),
    )
    .await;
    assert_eq!(
        responder_finalized.status, 200,
        "{}",
        responder_finalized.body
    );
    finalized
        .json()
        .get("peerTransport")
        .filter(|transport| !transport.is_null())
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "owner received no signed provider transport: {}",
                finalized.body
            )
        })
}

async fn export_recovery(owner: &NodeRuntime) -> (Vec<u8>, RecoveryUnlockKey) {
    let response = call(
        owner,
        "POST",
        "/api/v1/recovery/kit",
        Some(r#"{"confirmed":true}"#),
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.body);
    let response = response.json();
    (
        URL_SAFE_NO_PAD
            .decode(field(&response, "recoveryKit"))
            .expect("raw recovery kit"),
        RecoveryUnlockKey::from_base64(field(&response, "recoveryKey")).expect("recovery key"),
    )
}

/// Creates a signed provider entry in a kit, then stops both runtimes. The
/// returned socket is intentionally available for a UDP black-hole listener.
async fn paired_recovery_material(root: &TempDir) -> (Vec<u8>, RecoveryUnlockKey, SocketAddr) {
    let owner_path = root.path().join("owner");
    let provider_path = root.path().join("provider");
    let owner = start_node(&owner_path, "Lost owner", 0x71).await;
    let provider = start_node(&provider_path, "Surviving provider", 0x72).await;
    let transport = pair_owner_with_provider(&owner, &provider).await;
    let signed_address: SocketAddr = field(&transport, "address")
        .parse()
        .expect("signed provider endpoint");
    let (kit, unlock) = export_recovery(&owner).await;
    owner.stop().await.expect("stop owner");
    provider.stop().await.expect("stop provider");
    (kit, unlock, signed_address)
}

/// As above, but leave the genuine provider running so automatic recovery can
/// finish its empty-catalog pass before the explicit retry is exercised.
async fn paired_recovery_with_live_provider(
    root: &TempDir,
) -> (Vec<u8>, RecoveryUnlockKey, SocketAddr, NodeRuntime) {
    let owner_path = root.path().join("owner");
    let provider_path = root.path().join("provider");
    let owner = start_node(&owner_path, "Lost owner", 0x81).await;
    let provider = start_node(&provider_path, "Surviving provider", 0x82).await;
    let transport = pair_owner_with_provider(&owner, &provider).await;
    let signed_address: SocketAddr = field(&transport, "address")
        .parse()
        .expect("signed provider endpoint");
    let (kit, unlock) = export_recovery(&owner).await;
    owner.stop().await.expect("stop owner");
    (kit, unlock, signed_address, provider)
}

async fn wait_for_provider_request(socket: &UdpSocket) {
    let mut packet = [0_u8; 65_536];
    tokio::time::timeout(SHUTDOWN_DEADLINE, socket.recv_from(&mut packet))
        .await
        .expect("recovery provider request should begin before the shutdown assertion")
        .expect("read recovery provider packet");
}

async fn bind_silent_provider(address: SocketAddr) -> UdpSocket {
    tokio::time::timeout(SHUTDOWN_DEADLINE, async {
        loop {
            match UdpSocket::bind(address).await {
                Ok(socket) => return socket,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("bind silent provider endpoint: {error}"),
            }
        }
    })
    .await
    .expect("stopped provider must release its endpoint before the shutdown deadline")
}

async fn stop_before_provider_timeout(runtime: &NodeRuntime) {
    tokio::time::timeout(SHUTDOWN_DEADLINE, runtime.stop())
        .await
        .expect("stop must cancel the in-flight recovery request before its provider timeout")
        .expect("stop runtime");
}

async fn wait_for_recovery_phase(node: &NodeRuntime, phase: &str) {
    tokio::time::timeout(SHUTDOWN_DEADLINE, async {
        loop {
            let status = call(node, "GET", "/api/v1/recovery/status", None).await;
            assert_eq!(status.status, 200, "{}", status.body);
            if field(&status.json(), "phase") == phase {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("recovery did not reach {phase}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn normal_restart_repairs_recovery_handoff_after_core_recovery_before_runtime_setup() {
    let root = TempDir::new().expect("test root");
    let recovered_path = root.path().join("recovered");
    let (kit, unlock, _provider_address) = paired_recovery_material(&root).await;

    let options = EngineOptions::new(&recovered_path).with_key_protector(protector(0x73));
    let engine = Engine::recover_from_kit(options, &kit, &unlock).expect("recover core state");
    assert!(
        engine
            .config()
            .expect("recovered config")
            .recovery_bootstrap_pending
    );
    drop(engine);
    assert!(!recovered_path.join("provider-connections.json").exists());
    assert!(!recovered_path.join("recovery-state.json").exists());

    let repaired = NodeRuntime::start(runtime_configuration(&recovered_path, 0x73))
        .await
        .expect("normal restart repairs recovery handoff");
    let config: Value = serde_json::from_slice(
        &fs::read(recovered_path.join("config.json")).expect("repaired config"),
    )
    .expect("config JSON");
    assert_eq!(config["recoveryBootstrapPending"], false);
    assert!(recovered_path.join("provider-connections.json").is_file());
    let state: Value = serde_json::from_slice(
        &fs::read(recovered_path.join("recovery-state.json")).expect("durable recovery status"),
    )
    .expect("recovery state JSON");
    assert!(
        matches!(
            state["status"]["phase"].as_str(),
            Some("pending" | "blocked")
        ),
        "runtime must durably publish a recovery status before automatic retry: {state}"
    );
    repaired.stop().await.expect("stop repaired runtime");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_cancels_automatic_and_explicit_recovery_requests_before_provider_timeout() {
    let root = TempDir::new().expect("test root");
    let recovered_path = root.path().join("recovered");
    let (kit, unlock, provider_address) = paired_recovery_material(&root).await;
    let black_hole = bind_silent_provider(provider_address).await;

    let mut configuration = runtime_configuration(&recovered_path, 0x74);
    configuration.recovery = Some(RecoveryBootstrap {
        kit: Zeroizing::new(kit),
        unlock,
    });
    let automatic = NodeRuntime::start(configuration)
        .await
        .expect("start recovery runtime");
    wait_for_provider_request(&black_hole).await;
    stop_before_provider_timeout(&automatic).await;

    // A fresh runtime proves the recovery-store operation lock was released by
    // the cancelled automatic attempt.
    let reopened = NodeRuntime::start(runtime_configuration(&recovered_path, 0x74))
        .await
        .expect("restart after cancelled automatic recovery");
    stop_before_provider_timeout(&reopened).await;

    // For the explicit endpoint, let automatic recovery complete first with a
    // live paired provider. Then make that exact signed UDP endpoint silent;
    // the datagram below can therefore only come from POST /recovery/retry.
    let explicit_root = TempDir::new().expect("explicit retry root");
    let explicit_path = explicit_root.path().join("recovered");
    let (kit, unlock, explicit_provider_address, provider) =
        paired_recovery_with_live_provider(&explicit_root).await;
    let mut configuration = runtime_configuration(&explicit_path, 0x84);
    configuration.recovery = Some(RecoveryBootstrap {
        kit: Zeroizing::new(kit),
        unlock,
    });
    let explicit = NodeRuntime::start(configuration)
        .await
        .expect("start recovery runtime for explicit retry");
    wait_for_recovery_phase(&explicit, "no_catalogs").await;
    provider
        .stop()
        .await
        .expect("stop provider before explicit retry");
    drop(provider);
    let explicit_black_hole = bind_silent_provider(explicit_provider_address).await;
    let api_address = explicit.ready_info().api_address();
    let token = explicit.ready_info().api_token().expose().to_owned();
    let retry = tokio::spawn(async move {
        let _ = call_at(
            api_address,
            &token,
            "POST",
            "/api/v1/recovery/retry",
            Some(r#"{"confirmed":true}"#),
        )
        .await;
    });
    wait_for_provider_request(&explicit_black_hole).await;
    stop_before_provider_timeout(&explicit).await;
    tokio::time::timeout(SHUTDOWN_DEADLINE, retry)
        .await
        .expect("explicit retry HTTP worker joins after runtime stop")
        .expect("explicit retry task");
}
