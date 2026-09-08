//! Owner-loss recovery through two live loopback node runtimes.
//!
//! This is intentionally an HTTP/QUIC integration test rather than an engine
//! test. It proves that exported API material can recreate an owner after the
//! complete old state root is gone, that the recovered runtime reconnects to
//! the signed provider endpoint, and that recovery restores through that
//! provider without relying on any local owner chunks.

use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{RecoveryUnlockKey, StaticKeyProtector};
use covalent_node::runtime::{NodeRuntime, NodeRuntimeConfig, RecoveryBootstrap};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use zeroize::Zeroizing;

fn loopback_zero() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

fn protector(byte: u8) -> Arc<StaticKeyProtector> {
    Arc::new(StaticKeyProtector::new(1, [byte; 32]).expect("test protector"))
}

async fn start_node(path: &Path, name: &str, key: u8, peer_address: SocketAddr) -> NodeRuntime {
    let mut configuration = NodeRuntimeConfig::new(path, loopback_zero(), peer_address);
    configuration.device_name = name.to_owned();
    configuration.key_protector = Some(protector(key));
    configuration.advertised_peer_address = Some(peer_address);
    NodeRuntime::start(configuration)
        .await
        .expect("start live node runtime")
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

async fn call(node: &NodeRuntime, method: &str, path: &str, body: Option<&str>) -> HttpResponse {
    let ready = node.ready_info();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nConnection: close\r\n",
        ready.api_token().expose()
    );
    match body {
        Some(body) => request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )),
        None => request.push_str("Content-Length: 0\r\n\r\n"),
    }
    let mut stream = tokio::net::TcpStream::connect(ready.api_address())
        .await
        .expect("connect local API");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write local API request");
    stream.flush().await.expect("flush local API request");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read local API response");
    let raw = String::from_utf8(raw).expect("UTF-8 local API response");
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response {raw:?}"));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("missing HTTP status in {head:?}"));
    HttpResponse {
        status,
        body: body.to_owned(),
    }
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

async fn recovery_status(node: &NodeRuntime) -> Value {
    let response = call(node, "GET", "/api/v1/recovery/status", None).await;
    assert_eq!(response.status, 200, "{}", response.body);
    response.json()
}

async fn wait_for_recovery_phase(node: &NodeRuntime, phase: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let status = recovery_status(node).await;
            if field(&status, "phase") == phase {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("recovery did not reach {phase}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_loss_runtime_export_recovery_reconnects_and_restores_from_provider_only() {
    let root = TempDir::new().expect("test root");
    let owner_path = root.path().join("owner-lost-entirely");
    let provider_path = root.path().join("provider-survives");
    let recovered_path = root.path().join("recovered-owner");
    let source_path = root.path().join("source");
    let restore_path = root.path().join("restore");
    fs::create_dir_all(source_path.join("nested/empty")).expect("source empty directory");
    fs::write(
        source_path.join("nested/payload.bin"),
        b"owner-loss recovery bytes\0exact",
    )
    .expect("source payload");
    fs::create_dir_all(&restore_path).expect("restore root");

    let owner = start_node(&owner_path, "Lost owner", 0x51, loopback_zero()).await;
    let provider = start_node(&provider_path, "Surviving provider", 0x52, loopback_zero()).await;
    let provider_transport = pair_owner_with_provider(&owner, &provider).await;
    let provider_id = field(&provider_transport, "peerId").to_owned();
    let connected = call(
        &owner,
        "POST",
        "/api/v1/providers/connect",
        Some(&format!(r#"{{"peerTransport":{provider_transport}}}"#)),
    )
    .await;
    assert_eq!(connected.status, 200, "{}", connected.body);

    let backup = call(
        &owner,
        "POST",
        "/api/v1/backups",
        Some(&format!(
            r#"{{"sourceRoot":{},"displayName":"Owner loss evidence","snapshotId":"owner-loss-snapshot","jobId":"owner-loss-backup","selectedProviderIds":["{provider_id}"]}}"#,
            serde_json::to_string(&source_path).expect("source JSON")
        )),
    )
    .await;
    assert_eq!(backup.status, 200, "{}", backup.body);
    let backup = backup.json();
    assert_eq!(backup["selectedProviders"], 1);
    assert_eq!(backup["degradedFailures"], 0);
    let backup_id = field(&backup, "backupId").to_owned();

    let exported = call(
        &owner,
        "POST",
        "/api/v1/recovery/kit",
        Some(r#"{"confirmed":true}"#),
    )
    .await;
    assert_eq!(exported.status, 200, "{}", exported.body);
    let exported = exported.json();
    let kit = URL_SAFE_NO_PAD
        .decode(field(&exported, "recoveryKit"))
        .expect("raw recovery kit");
    let unlock =
        RecoveryUnlockKey::from_base64(field(&exported, "recoveryKey")).expect("recovery key");

    let provider_peer_address = provider.ready_info().peer_address();
    owner.stop().await.expect("stop lost owner");
    fs::remove_dir_all(&owner_path).expect("remove the complete owner state root");
    assert!(
        !owner_path.exists(),
        "no owner identity, keys, chunks, or catalog remain"
    );

    // The initial automatic import must remain conservative while the only
    // signed provider is offline. Restarting it at its original signed socket
    // proves the subsequent retry uses the recovered transport rather than a
    // hand-built replacement endpoint.
    provider.stop().await.expect("take provider offline");
    let mut recovery_configuration =
        NodeRuntimeConfig::new(&recovered_path, loopback_zero(), loopback_zero());
    recovery_configuration.key_protector = Some(protector(0x53));
    recovery_configuration.advertised_peer_address = Some(loopback_zero());
    recovery_configuration.recovery = Some(RecoveryBootstrap {
        kit: Zeroizing::new(kit),
        unlock,
    });
    let recovered = NodeRuntime::start(recovery_configuration)
        .await
        .expect("start recovered runtime");
    let offline = wait_for_recovery_phase(&recovered, "blocked").await;
    assert_eq!(offline["newerSnapshotMayExist"], true);
    assert_eq!(
        offline["configuredProviderIds"],
        serde_json::json!([provider_id])
    );

    let healed_provider = start_node(
        &provider_path,
        "Surviving provider",
        0x52,
        provider_peer_address,
    )
    .await;
    let retried = call(
        &recovered,
        "POST",
        "/api/v1/recovery/retry",
        Some(r#"{"confirmed":true}"#),
    )
    .await;
    assert_eq!(retried.status, 200, "{}", retried.body);
    let imported = wait_for_recovery_phase(&recovered, "imported").await;
    assert_eq!(imported["newerSnapshotMayExist"], false);
    assert_eq!(
        field(
            imported["recoveredBackups"]
                .as_array()
                .and_then(|backups| backups.first())
                .expect("one recovered backup"),
            "backupId"
        ),
        backup_id
    );

    let backups = call(&recovered, "GET", "/api/v1/backups", None).await;
    assert_eq!(backups.status, 200, "{}", backups.body);
    assert!(
        backups
            .json()
            .as_array()
            .expect("backup list")
            .iter()
            .any(|backup| field(backup, "backupId") == backup_id),
        "recovered backup is visible through the runtime API"
    );

    let preview = call(
        &recovered,
        "POST",
        "/api/v1/restores/preview",
        Some(&format!(
            r#"{{"backupId":"{backup_id}","snapshotId":"owner-loss-snapshot","targetRoot":{},"conflictPolicy":"fail","jobId":"owner-loss-restore"}}"#,
            serde_json::to_string(&restore_path).expect("restore JSON")
        )),
    )
    .await;
    assert_eq!(preview.status, 200, "{}", preview.body);
    let preview = preview.json();
    let plan_id = field(&preview, "planId").to_owned();
    let executed = call(
        &recovered,
        "POST",
        "/api/v1/restores/execute",
        Some(&format!(r#"{{"planId":"{plan_id}"}}"#)),
    )
    .await;
    assert_eq!(executed.status, 200, "{}", executed.body);
    assert_eq!(
        fs::read(restore_path.join("nested/payload.bin")).expect("provider-only restored bytes"),
        b"owner-loss recovery bytes\0exact"
    );
    assert!(restore_path.join("nested/empty").is_dir());

    recovered.stop().await.expect("stop recovered runtime");
    healed_provider.stop().await.expect("stop healed provider");
}
