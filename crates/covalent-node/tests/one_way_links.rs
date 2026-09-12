//! Real three-node one-way folder-link integration coverage.
//!
//! Run explicitly with maintained test executables:
//! `COVALENT_TEST_SYNC_WORKER=/absolute/worker COVALENT_TEST_SYNC_GUARDIAN=/absolute/guardian cargo test -p covalent-node --test one_way_links -- --ignored --exact three_node_one_way_links_enforce_deletions_restore_and_shared_settings --nocapture`

use std::fs::{self, DirBuilder};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use covalent_core::StaticKeyProtector;
use covalent_node::runtime::{NodeRuntime, NodeRuntimeConfig};
use covalent_node::sync_engine::{FolderSyncRuntimeConfig, VerifiedEngineExecutable};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use uuid::Uuid;

const WAIT_LIMIT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone)]
struct TestBinaries {
    worker: PathBuf,
    worker_sha256: [u8; 32],
    guardian: PathBuf,
    guardian_sha256: [u8; 32],
}

struct NodeSpec {
    data_directory: PathBuf,
    runtime_parent: PathBuf,
    device_name: &'static str,
    key_byte: u8,
    peer_address: SocketAddr,
    sync_address: SocketAddr,
}

impl NodeSpec {
    fn new(root: &Path, name: &'static str, key_byte: u8) -> Self {
        let data_directory = root.join(name);
        let runtime_parent = root.join(format!("{name}-runtime"));
        private_directory(&data_directory);
        private_directory(&runtime_parent);
        Self {
            data_directory,
            runtime_parent,
            device_name: name,
            key_byte,
            peer_address: loopback_zero(),
            sync_address: reserve_loopback(),
        }
    }
}

fn loopback_zero() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

fn reserve_loopback() -> SocketAddr {
    let listener = TcpListener::bind(loopback_zero()).expect("reserve loopback port");
    let address = listener.local_addr().expect("reserved loopback address");
    drop(listener);
    address
}

fn private_directory(path: &Path) {
    DirBuilder::new()
        .mode(0o700)
        .create(path)
        .unwrap_or_else(|error| panic!("create private directory {}: {error}", path.display()));
}

fn executable_from_env(name: &str) -> (PathBuf, [u8; 32]) {
    let path = PathBuf::from(
        std::env::var(name).unwrap_or_else(|_| panic!("{name} must name an absolute executable")),
    );
    assert!(
        path.is_absolute(),
        "{name} must be absolute: {}",
        path.display()
    );
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| panic!("read {name} executable {}: {error}", path.display()));
    (path, Sha256::digest(bytes).into())
}

fn test_binaries() -> TestBinaries {
    let (worker, worker_sha256) = executable_from_env("COVALENT_TEST_SYNC_WORKER");
    let (guardian, guardian_sha256) = executable_from_env("COVALENT_TEST_SYNC_GUARDIAN");
    TestBinaries {
        worker,
        worker_sha256,
        guardian,
        guardian_sha256,
    }
}

fn verified(path: &Path, sha256: [u8; 32]) -> VerifiedEngineExecutable {
    VerifiedEngineExecutable::open(path, sha256)
        .unwrap_or_else(|error| panic!("verify executable {}: {error}", path.display()))
}

async fn start_node(spec: &NodeSpec, binaries: &TestBinaries) -> NodeRuntime {
    let mut configuration =
        NodeRuntimeConfig::new(&spec.data_directory, loopback_zero(), spec.peer_address);
    configuration.device_name = spec.device_name.to_owned();
    configuration.key_protector = Some(Arc::new(
        StaticKeyProtector::new(1, [spec.key_byte; 32]).expect("test key protector"),
    ));
    configuration.advertised_peer_address = Some(spec.peer_address);
    configuration.folder_sync = Some(FolderSyncRuntimeConfig {
        guardian: verified(&binaries.guardian, binaries.guardian_sha256),
        worker: verified(&binaries.worker, binaries.worker_sha256),
        runtime_parent: spec.runtime_parent.clone(),
        listener: spec.sync_address,
        advertised_address: Some(spec.sync_address),
    });
    NodeRuntime::start(configuration)
        .await
        .unwrap_or_else(|error| panic!("start {} runtime: {error:#}", spec.device_name))
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

async fn call(node: &NodeRuntime, method: &str, path: &str, body: Option<&Value>) -> HttpResponse {
    let body = body.map(Value::to_string);
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nConnection: close\r\n",
        node.ready_info().api_token().expose()
    );
    match body.as_deref() {
        Some(body) => request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )),
        None => request.push_str("Content-Length: 0\r\n\r\n"),
    }
    let mut stream = tokio::net::TcpStream::connect(node.ready_info().api_address())
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
    let raw = String::from_utf8_lossy(&raw);
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .expect("well-formed HTTP response");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .expect("HTTP response status");
    HttpResponse {
        status,
        body: body.to_owned(),
    }
}

async fn post_ok(node: &NodeRuntime, path: &str, body: Value) -> Value {
    let response = call(node, "POST", path, Some(&body)).await;
    assert_eq!(response.status, 200, "POST {path}: {}", response.body);
    response.json()
}

async fn status(node: &NodeRuntime) -> Value {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let response = call(node, "GET", "/api/v1/sync/status", None).await;
        if response.status == 200 {
            return response.json();
        }
        let retryable_busy = response.status == 503
            && response.json().get("code").and_then(Value::as_str) == Some("folder_sync_busy");
        assert!(
            retryable_busy && Instant::now() < deadline,
            "sync status {}: {}",
            response.status,
            response.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn string_field<'a>(value: &'a Value, name: &str) -> &'a str {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {name} in {value}"))
}

async fn pair(source: &NodeRuntime, destination: &NodeRuntime, name: &str) -> String {
    let invitation = post_ok(
        source,
        "/api/v1/pair/invitations",
        json!({
            "lifetimeMs": 600_000,
            "endpoints": [source.ready_info().peer_address().to_string()],
        }),
    )
    .await;
    let accepted = post_ok(
        destination,
        "/api/v1/pair/accept",
        json!({
            "invitation": invitation,
            "responderName": name,
            "responderRoles": ["storage_provider", "backup_reader"],
            "inviterRoles": ["backup_writer", "backup_reader"],
        }),
    )
    .await;
    let code = string_field(&accepted, "authenticationString").to_owned();
    let session = post_ok(
        destination,
        "/api/v1/pair/confirm/responder",
        json!({"session": accepted, "displayedCode": code}),
    )
    .await;
    let session = post_ok(
        source,
        "/api/v1/pair/confirm/inviter",
        json!({"session": session, "displayedCode": code}),
    )
    .await;
    let finalized = post_ok(
        source,
        "/api/v1/pair/finalize/inviter",
        json!({"session": session}),
    )
    .await;
    post_ok(
        destination,
        "/api/v1/pair/finalize/responder",
        json!({"session": session}),
    )
    .await;
    string_field(
        finalized
            .get("peerTransport")
            .filter(|value| !value.is_null())
            .unwrap_or_else(|| panic!("missing signed peer transport in {finalized}")),
        "peerId",
    )
    .to_owned()
}

async fn wait_for_status(
    node: &NodeRuntime,
    description: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        let last = status(node).await;
        if predicate(&last) {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}; last status: {last}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn folder_shares(status: &Value, folder_id: Uuid) -> Vec<&Value> {
    status
        .get("shares")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing shares in {status}"))
        .iter()
        .filter(|share| {
            share.get("folderId").and_then(Value::as_str) == Some(&folder_id.to_string())
        })
        .collect()
}

fn policy_matches(value: &Value, propagate: bool, restore: bool) -> bool {
    value
        .get("propagateSourceDeletions")
        .and_then(Value::as_bool)
        == Some(propagate)
        && value.get("restoreLocalDeletions").and_then(Value::as_bool) == Some(restore)
}

fn shares_ready_at(
    status: &Value,
    folder_id: Uuid,
    count: usize,
    revision: u64,
    propagate: bool,
    restore: bool,
) -> bool {
    let shares = folder_shares(status, folder_id);
    shares.len() == count
        && shares.iter().all(|share| {
            share.get("phase").and_then(Value::as_str) == Some("ready")
                && share
                    .get("linkPolicy")
                    .is_some_and(|policy| policy_matches(policy, propagate, restore))
                && share.get("linkSettings").is_some_and(|state| {
                    state.get("revision").and_then(Value::as_u64) == Some(revision)
                        && state.get("confirmed").and_then(Value::as_bool) == Some(true)
                        && state.get("pendingChange").is_some_and(Value::is_null)
                        && state.get("conflictedChange").is_some_and(Value::is_null)
                        && state
                            .pointer("/settings/deletionPolicy")
                            .is_some_and(|policy| policy_matches(policy, propagate, restore))
                })
        })
}

fn shares_converged_after_competing_edits(
    status: &Value,
    folder_id: Uuid,
    count: usize,
    committed_change_id: Uuid,
    propagate: bool,
    restore: bool,
    conflict: Option<(Uuid, bool, bool)>,
) -> bool {
    let committed_change_id = committed_change_id.to_string();
    let shares = folder_shares(status, folder_id);
    shares.len() == count
        && shares.iter().all(|share| {
            let Some(state) = share.get("linkSettings") else {
                return false;
            };
            let conflict_matches = match conflict {
                None => state.get("conflictedChange").is_some_and(Value::is_null),
                Some((change_id, conflict_propagate, conflict_restore)) => {
                    state.get("conflictedChange").is_some_and(|request| {
                        request.get("changeId").and_then(Value::as_str)
                            == Some(change_id.to_string().as_str())
                            && request.get("expectedRevision").and_then(Value::as_u64) == Some(4)
                            && request
                                .pointer("/settings/deletionPolicy")
                                .is_some_and(|policy| {
                                    policy_matches(policy, conflict_propagate, conflict_restore)
                                })
                    })
                }
            };
            share.get("phase").and_then(Value::as_str) == Some("ready")
                && share
                    .get("linkPolicy")
                    .is_some_and(|policy| policy_matches(policy, propagate, restore))
                && state.get("revision").and_then(Value::as_u64) == Some(5)
                && state.get("changeId").and_then(Value::as_str)
                    == Some(committed_change_id.as_str())
                && state.get("confirmed").and_then(Value::as_bool) == Some(true)
                && state.get("pendingChange").is_some_and(Value::is_null)
                && state
                    .pointer("/settings/deletionPolicy")
                    .is_some_and(|policy| policy_matches(policy, propagate, restore))
                && conflict_matches
        })
}

async fn wait_ready_at(
    node: &NodeRuntime,
    description: &str,
    folder_id: Uuid,
    count: usize,
    revision: u64,
    propagate: bool,
    restore: bool,
) {
    wait_for_status(node, description, |value| {
        shares_ready_at(value, folder_id, count, revision, propagate, restore)
    })
    .await;
}

async fn offer_and_accept(
    source: &NodeRuntime,
    destination: &NodeRuntime,
    peer_id: &str,
    folder_id: Uuid,
    source_root: &Path,
    destination_root: &Path,
) {
    let offered = post_ok(
        source,
        "/api/v1/sync/folders",
        json!({
            "peerId": peer_id,
            "folderId": folder_id,
            "label": "Three-node one-way link",
            "selectedRoot": source_root,
            "linkPolicy": {
                "propagateSourceDeletions": false,
                "restoreLocalDeletions": false,
            },
        }),
    )
    .await;
    let offer_id = string_field(&offered, "offerId").to_owned();
    wait_for_status(destination, "signed incoming folder offer", |value| {
        folder_shares(value, folder_id).iter().any(|share| {
            share.get("offerId").and_then(Value::as_str) == Some(offer_id.as_str())
                && share.get("incoming").and_then(Value::as_bool) == Some(true)
        })
    })
    .await;
    post_ok(
        destination,
        "/api/v1/sync/accept",
        json!({"offerId": offer_id, "selectedRoot": destination_root}),
    )
    .await;
}

async fn update_settings(
    node: &NodeRuntime,
    folder_id: Uuid,
    expected_revision: u64,
    change_id: Uuid,
    propagate: bool,
    restore: bool,
) {
    post_ok(
        node,
        "/api/v1/sync/settings",
        json!({
            "folderId": folder_id,
            "changeId": change_id,
            "expectedRevision": expected_revision,
            "settings": {
                "deletionPolicy": {
                    "propagateSourceDeletions": propagate,
                    "restoreLocalDeletions": restore,
                },
                "paused": false,
            },
        }),
    )
    .await;
}

async fn wait_file(path: &Path, expected: &[u8], description: &str) {
    assert!(
        wait_file_until(path, expected).await,
        "timed out waiting for {description}: {}",
        path.display()
    );
}

async fn wait_file_until(path: &Path, expected: &[u8]) -> bool {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        match fs::read(path) {
            Ok(bytes) if bytes == expected => return true,
            _ => {}
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn wait_absent(path: &Path, description: &str) {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}: {}",
            path.display()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn write(root: &Path, name: &str, bytes: &[u8]) {
    fs::write(root.join(name), bytes)
        .unwrap_or_else(|error| panic!("write {name} in {}: {error}", root.display()));
}

fn assert_absent(path: &Path, description: &str) {
    assert!(
        matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
        "{description}: {} exists",
        path.display()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit maintained folder-sync worker and freshly compiled guardian paths"]
async fn three_node_one_way_links_enforce_deletions_restore_and_shared_settings() {
    let binaries = test_binaries();
    let root = TempDir::new().expect("isolated three-node test root");
    let mut source_spec = NodeSpec::new(root.path(), "source", 0x91);
    let destination_a_spec = NodeSpec::new(root.path(), "destination-a", 0x92);
    let destination_b_spec = NodeSpec::new(root.path(), "destination-b", 0x93);
    let source_root = root.path().join("source-files");
    let destination_a_root = root.path().join("destination-a-files");
    let destination_b_root = root.path().join("destination-b-files");
    for path in [&source_root, &destination_a_root, &destination_b_root] {
        private_directory(path);
    }

    let mut source = start_node(&source_spec, &binaries).await;
    source_spec.peer_address = source.ready_info().peer_address();
    let destination_a = start_node(&destination_a_spec, &binaries).await;
    let destination_b = start_node(&destination_b_spec, &binaries).await;

    let destination_a_id = pair(&source, &destination_a, "Destination A").await;
    let destination_b_id = pair(&source, &destination_b, "Destination B").await;
    let folder_id = Uuid::new_v4();
    offer_and_accept(
        &source,
        &destination_a,
        &destination_a_id,
        folder_id,
        &source_root,
        &destination_a_root,
    )
    .await;
    offer_and_accept(
        &source,
        &destination_b,
        &destination_b_id,
        folder_id,
        &source_root,
        &destination_b_root,
    )
    .await;
    wait_ready_at(
        &source,
        "source fanout ready",
        folder_id,
        2,
        0,
        false,
        false,
    )
    .await;
    wait_ready_at(
        &destination_a,
        "destination A ready",
        folder_id,
        1,
        0,
        false,
        false,
    )
    .await;
    wait_ready_at(
        &destination_b,
        "destination B ready",
        folder_id,
        1,
        0,
        false,
        false,
    )
    .await;

    write(&source_root, "initial.txt", b"source initial\n");
    wait_file(
        &destination_a_root.join("initial.txt"),
        b"source initial\n",
        "initial transfer to destination A",
    )
    .await;
    wait_file(
        &destination_b_root.join("initial.txt"),
        b"source initial\n",
        "initial transfer to destination B",
    )
    .await;

    write(
        &destination_a_root,
        "destination-only.txt",
        b"must stay local\n",
    );
    write(&source_root, "destination-barrier.txt", b"barrier one\n");
    wait_file(
        &destination_a_root.join("destination-barrier.txt"),
        b"barrier one\n",
        "destination A barrier after its local write",
    )
    .await;
    wait_file(
        &destination_b_root.join("destination-barrier.txt"),
        b"barrier one\n",
        "destination B barrier after destination A local write",
    )
    .await;
    assert_absent(
        &source_root.join("destination-only.txt"),
        "destination-created file reached source",
    );
    assert_absent(
        &destination_b_root.join("destination-only.txt"),
        "destination-created file reached another destination",
    );

    write(&source_root, "local-delete.txt", b"source version one\n");
    wait_file(
        &destination_a_root.join("local-delete.txt"),
        b"source version one\n",
        "local-deletion seed at destination A",
    )
    .await;
    wait_file(
        &destination_b_root.join("local-delete.txt"),
        b"source version one\n",
        "local-deletion seed at destination B",
    )
    .await;
    fs::remove_file(destination_a_root.join("local-delete.txt"))
        .expect("delete destination A local copy");
    write(
        &source_root,
        "local-delete.txt",
        b"source version two is different\n",
    );
    write(&source_root, "local-delete-barrier.txt", b"barrier two\n");
    wait_file(
        &destination_b_root.join("local-delete.txt"),
        b"source version two is different\n",
        "source edit at destination B",
    )
    .await;
    wait_file(
        &destination_a_root.join("local-delete-barrier.txt"),
        b"barrier two\n",
        "destination A barrier after source edit",
    )
    .await;
    assert_absent(
        &destination_a_root.join("local-delete.txt"),
        "locally deleted destination file was restored without restore policy",
    );

    write(
        &source_root,
        "keep-after-source-delete.txt",
        b"keep this copy\n",
    );
    wait_file(
        &destination_a_root.join("keep-after-source-delete.txt"),
        b"keep this copy\n",
        "kept-copy seed at destination A",
    )
    .await;
    wait_file(
        &destination_b_root.join("keep-after-source-delete.txt"),
        b"keep this copy\n",
        "kept-copy seed at destination B",
    )
    .await;
    fs::remove_file(source_root.join("keep-after-source-delete.txt"))
        .expect("delete source file under keep policy");
    write(
        &source_root,
        "source-delete-barrier.txt",
        b"barrier three\n",
    );
    wait_file(
        &destination_a_root.join("source-delete-barrier.txt"),
        b"barrier three\n",
        "destination A barrier after kept source deletion",
    )
    .await;
    wait_file(
        &destination_b_root.join("source-delete-barrier.txt"),
        b"barrier three\n",
        "destination B barrier after kept source deletion",
    )
    .await;
    assert_eq!(
        fs::read(destination_a_root.join("keep-after-source-delete.txt"))
            .expect("destination A retained source deletion"),
        b"keep this copy\n"
    );
    assert_eq!(
        fs::read(destination_b_root.join("keep-after-source-delete.txt"))
            .expect("destination B retained source deletion"),
        b"keep this copy\n"
    );

    update_settings(&source, folder_id, 0, Uuid::new_v4(), true, false).await;
    wait_ready_at(&source, "source revision 1", folder_id, 2, 1, true, false).await;
    wait_ready_at(
        &destination_a,
        "destination A revision 1",
        folder_id,
        1,
        1,
        true,
        false,
    )
    .await;
    wait_ready_at(
        &destination_b,
        "destination B revision 1",
        folder_id,
        1,
        1,
        true,
        false,
    )
    .await;
    assert_absent(
        &destination_a_root.join("local-delete.txt"),
        "destination-local deletion did not survive the revision 1 worker restart",
    );
    assert_eq!(
        fs::read(source_root.join("local-delete.txt"))
            .expect("source update after revision 1 restart"),
        b"source version two is different\n"
    );
    assert_eq!(
        fs::read(destination_b_root.join("local-delete.txt"))
            .expect("destination B update after revision 1 restart"),
        b"source version two is different\n"
    );

    write(
        &source_root,
        "propagated-delete.txt",
        b"delete everywhere\n",
    );
    wait_file(
        &destination_a_root.join("propagated-delete.txt"),
        b"delete everywhere\n",
        "propagated-deletion seed at destination A",
    )
    .await;
    wait_file(
        &destination_b_root.join("propagated-delete.txt"),
        b"delete everywhere\n",
        "propagated-deletion seed at destination B",
    )
    .await;
    fs::remove_file(source_root.join("propagated-delete.txt"))
        .expect("delete source file under propagation policy");
    write(&source_root, "propagation-barrier.txt", b"barrier four\n");
    wait_file(
        &destination_a_root.join("propagation-barrier.txt"),
        b"barrier four\n",
        "destination A barrier after propagated deletion",
    )
    .await;
    wait_file(
        &destination_b_root.join("propagation-barrier.txt"),
        b"barrier four\n",
        "destination B barrier after propagated deletion",
    )
    .await;
    wait_absent(
        &destination_a_root.join("propagated-delete.txt"),
        "propagated deletion at destination A",
    )
    .await;
    wait_absent(
        &destination_b_root.join("propagated-delete.txt"),
        "propagated deletion at destination B",
    )
    .await;

    let restore_bytes = b"restore without source edit\n";
    let source_restore_file = source_root.join("restore-toggle.txt");
    let destination_a_restore_file = destination_a_root.join("restore-toggle.txt");
    write(&source_root, "restore-toggle.txt", restore_bytes);
    wait_file(
        &destination_a_restore_file,
        restore_bytes,
        "restore seed at destination A",
    )
    .await;
    wait_file(
        &destination_b_root.join("restore-toggle.txt"),
        restore_bytes,
        "restore seed at destination B",
    )
    .await;
    fs::remove_file(&destination_a_restore_file)
        .expect("delete destination A copy before restore toggle");
    assert_absent(
        &destination_a_restore_file,
        "destination A restore seed deletion",
    );
    update_settings(&source, folder_id, 1, Uuid::new_v4(), true, true).await;
    wait_ready_at(&source, "source revision 2", folder_id, 2, 2, true, true).await;
    wait_ready_at(
        &destination_a,
        "destination A revision 2",
        folder_id,
        1,
        2,
        true,
        true,
    )
    .await;
    wait_ready_at(
        &destination_b,
        "destination B revision 2",
        folder_id,
        1,
        2,
        true,
        true,
    )
    .await;
    let restored = wait_file_until(&destination_a_restore_file, restore_bytes).await;
    assert_eq!(
        fs::read(&source_restore_file).expect("source restore file remains readable"),
        restore_bytes,
        "source restore file changed during destination restart"
    );
    assert!(
        restored,
        "background restore did not recover the destination file after confirmed revision 2"
    );

    update_settings(&destination_a, folder_id, 2, Uuid::new_v4(), false, false).await;
    wait_ready_at(
        &source,
        "source accepted destination revision 3",
        folder_id,
        2,
        3,
        false,
        false,
    )
    .await;
    wait_ready_at(
        &destination_a,
        "requesting destination converged at revision 3",
        folder_id,
        1,
        3,
        false,
        false,
    )
    .await;
    wait_ready_at(
        &destination_b,
        "second destination converged at revision 3",
        folder_id,
        1,
        3,
        false,
        false,
    )
    .await;

    source
        .stop()
        .await
        .expect("stop source before offline edit");
    let offline_change_id = Uuid::new_v4();
    update_settings(&destination_a, folder_id, 3, offline_change_id, false, true).await;
    wait_for_status(
        &destination_a,
        "durable pending offline settings request",
        |value| {
            let shares = folder_shares(value, folder_id);
            shares.len() == 1
                && shares[0]
                    .pointer("/linkSettings/pendingChange/changeId")
                    .and_then(Value::as_str)
                    == Some(&offline_change_id.to_string())
        },
    )
    .await;
    drop(source);
    source = start_node(&source_spec, &binaries).await;
    assert_eq!(
        source.ready_info().peer_address(),
        source_spec.peer_address,
        "source restarted on its signed peer endpoint"
    );
    wait_ready_at(
        &source,
        "restarted source revision 4",
        folder_id,
        2,
        4,
        false,
        true,
    )
    .await;
    wait_ready_at(
        &destination_a,
        "offline requester converged at revision 4",
        folder_id,
        1,
        4,
        false,
        true,
    )
    .await;
    wait_ready_at(
        &destination_b,
        "other destination converged after source restart",
        folder_id,
        1,
        4,
        false,
        true,
    )
    .await;

    source
        .stop()
        .await
        .expect("stop source before competing offline edits");
    let destination_a_change = Uuid::new_v4();
    let destination_b_change = Uuid::new_v4();
    update_settings(
        &destination_a,
        folder_id,
        4,
        destination_a_change,
        true,
        false,
    )
    .await;
    update_settings(
        &destination_b,
        folder_id,
        4,
        destination_b_change,
        false,
        false,
    )
    .await;
    for (node, description, change_id, propagate) in [
        (
            &destination_a,
            "destination A competing request is pending",
            destination_a_change,
            true,
        ),
        (
            &destination_b,
            "destination B competing request is pending",
            destination_b_change,
            false,
        ),
    ] {
        wait_for_status(node, description, |value| {
            let shares = folder_shares(value, folder_id);
            shares.len() == 1
                && shares[0]
                    .pointer("/linkSettings/pendingChange")
                    .is_some_and(|request| {
                        request.get("changeId").and_then(Value::as_str)
                            == Some(change_id.to_string().as_str())
                            && request.get("expectedRevision").and_then(Value::as_u64) == Some(4)
                            && request
                                .pointer("/settings/deletionPolicy")
                                .is_some_and(|policy| policy_matches(policy, propagate, false))
                    })
        })
        .await;
    }

    drop(source);
    source = start_node(&source_spec, &binaries).await;
    assert_eq!(
        source.ready_info().peer_address(),
        source_spec.peer_address,
        "source restarted on its signed peer endpoint after competing edits"
    );
    let source_status = wait_for_status(&source, "one competing edit wins revision 5", |value| {
        shares_converged_after_competing_edits(
            value,
            folder_id,
            2,
            destination_a_change,
            true,
            false,
            None,
        ) || shares_converged_after_competing_edits(
            value,
            folder_id,
            2,
            destination_b_change,
            false,
            false,
            None,
        )
    })
    .await;
    let committed_change_id = Uuid::parse_str(
        folder_shares(&source_status, folder_id)[0]
            .pointer("/linkSettings/changeId")
            .and_then(Value::as_str)
            .expect("source revision 5 change ID"),
    )
    .expect("source revision 5 UUID");
    let destination_a_won = committed_change_id == destination_a_change;
    assert!(
        destination_a_won || committed_change_id == destination_b_change,
        "source committed neither competing request"
    );
    let (winner_propagate, loser_change, loser_propagate) = if destination_a_won {
        (true, destination_b_change, false)
    } else {
        (false, destination_a_change, true)
    };
    wait_for_status(
        &destination_a,
        "destination A observes the CAS winner",
        |value| {
            shares_converged_after_competing_edits(
                value,
                folder_id,
                1,
                committed_change_id,
                winner_propagate,
                false,
                (!destination_a_won).then_some((loser_change, loser_propagate, false)),
            )
        },
    )
    .await;
    wait_for_status(
        &destination_b,
        "destination B observes the CAS winner",
        |value| {
            shares_converged_after_competing_edits(
                value,
                folder_id,
                1,
                committed_change_id,
                winner_propagate,
                false,
                destination_a_won.then_some((loser_change, loser_propagate, false)),
            )
        },
    )
    .await;
    assert!(
        shares_converged_after_competing_edits(
            &status(&source).await,
            folder_id,
            2,
            committed_change_id,
            winner_propagate,
            false,
            None,
        ),
        "the losing request overwrote the revision 5 winner"
    );

    destination_b
        .stop()
        .await
        .expect("stop destination B before independent fanout");
    write(
        &source_root,
        "offline-destination.txt",
        b"online destination continues\n",
    );
    wait_file(
        &destination_a_root.join("offline-destination.txt"),
        b"online destination continues\n",
        "online destination while another destination is offline",
    )
    .await;
    assert_absent(
        &destination_b_root.join("offline-destination.txt"),
        "stopped destination unexpectedly transferred",
    );
    source.stop().await.expect("stop source");
    destination_a.stop().await.expect("stop destination A");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit maintained folder-sync worker and freshly compiled guardian paths"]
async fn collection_links_isolate_sources_and_preserve_files_when_a_source_cannot_scan() {
    let binaries = test_binaries();
    let root = TempDir::new().expect("isolated collection test root");
    let mut a_spec = NodeSpec::new(root.path(), "family-a", 0xa1);
    let b_spec = NodeSpec::new(root.path(), "family-b", 0xa2);
    let pool_spec = NodeSpec::new(root.path(), "collection", 0xa3);
    let a_root = root.path().join("a-photos");
    let b_root = root.path().join("b-photos");
    let collection = root.path().join("family-photos");
    let a_target = collection.join("Family A");
    let b_target = collection.join("Family B");
    for path in [&a_root, &b_root, &collection, &a_target, &b_target] {
        private_directory(path);
    }
    let mut a = start_node(&a_spec, &binaries).await;
    a_spec.peer_address = a.ready_info().peer_address();
    let b = start_node(&b_spec, &binaries).await;
    let pool = start_node(&pool_spec, &binaries).await;
    let a_peer = pair(&a, &pool, "Family collection A").await;
    let b_peer = pair(&b, &pool, "Family collection B").await;
    let a_folder = Uuid::new_v4();
    let b_folder = Uuid::new_v4();
    offer_and_accept(&a, &pool, &a_peer, a_folder, &a_root, &a_target).await;
    let offered = post_ok(
        &b,
        "/api/v1/sync/folders",
        json!({
            "peerId": b_peer,
            "folderId": b_folder,
            "label": "Family B photos",
            "selectedRoot": b_root,
            "linkPolicy": {
                "propagateSourceDeletions": false,
                "restoreLocalDeletions": false,
            },
        }),
    )
    .await;
    let offer_id = string_field(&offered, "offerId");
    wait_for_status(&pool, "second collection offer", |value| {
        folder_shares(value, b_folder)
            .iter()
            .any(|share| share.get("offerId").and_then(Value::as_str) == Some(offer_id))
    })
    .await;
    // Two links cannot share a root or claim the parent of another link.
    for rejected_root in [&a_target, &collection] {
        let response = call(
            &pool,
            "POST",
            "/api/v1/sync/accept",
            Some(&json!({"offerId": offer_id, "selectedRoot": rejected_root})),
        )
        .await;
        assert_eq!(
            response.status, 409,
            "overlapping collection root accepted: {}",
            response.body
        );
    }
    post_ok(
        &pool,
        "/api/v1/sync/accept",
        json!({"offerId": offer_id, "selectedRoot": b_target}),
    )
    .await;
    wait_ready_at(&pool, "family A link ready", a_folder, 1, 0, false, false).await;
    wait_ready_at(&pool, "family B link ready", b_folder, 1, 0, false, false).await;
    write(&a_root, "IMG_0001.jpg", b"family A photo\n");
    write(&a_root, "a-only.jpg", b"only family A\n");
    write(&b_root, "IMG_0001.jpg", b"family B photo\n");
    write(&b_root, "b-only.jpg", b"only family B\n");
    for (path, bytes) in [
        (
            a_target.join("IMG_0001.jpg"),
            b"family A photo\n".as_slice(),
        ),
        (a_target.join("a-only.jpg"), b"only family A\n".as_slice()),
        (
            b_target.join("IMG_0001.jpg"),
            b"family B photo\n".as_slice(),
        ),
        (b_target.join("b-only.jpg"), b"only family B\n".as_slice()),
    ] {
        wait_file(&path, bytes, "isolated collection transfer").await;
    }
    for path in [
        a_root.join("b-only.jpg"),
        b_root.join("a-only.jpg"),
        a_target.join("b-only.jpg"),
        b_target.join("a-only.jpg"),
    ] {
        assert_absent(&path, "another link's photo crossed collection boundaries");
    }

    update_settings(&a, a_folder, 0, Uuid::new_v4(), true, false).await;
    wait_ready_at(
        &a,
        "family A propagation enabled",
        a_folder,
        1,
        1,
        true,
        false,
    )
    .await;
    wait_ready_at(
        &pool,
        "collection confirms family A propagation",
        a_folder,
        1,
        1,
        true,
        false,
    )
    .await;
    assert!(
        shares_ready_at(&status(&b).await, b_folder, 1, 0, false, false),
        "family A settings changed family B"
    );

    a.stop()
        .await
        .expect("stop family A before missing-mount simulation");
    drop(a);
    let saved_root = root.path().join("a-original-mount");
    fs::rename(&a_root, &saved_root).expect("detach selected source root");
    private_directory(&a_root);
    a = start_node(&a_spec, &binaries).await;
    wait_for_status(&a, "empty replacement root rejected", |value| {
        value.get("lifecycle").and_then(Value::as_str) == Some("needsAttention")
            && value.get("issue").and_then(Value::as_str) == Some("journal")
    })
    .await;
    write(&b_root, "online.jpg", b"family B continues\n");
    wait_file(
        &b_target.join("online.jpg"),
        b"family B continues\n",
        "healthy collection link while another source is unavailable",
    )
    .await;
    assert_eq!(
        fs::read(a_target.join("IMG_0001.jpg")).unwrap(),
        b"family A photo\n",
        "empty replacement source deleted destination files"
    );

    a.stop().await.expect("stop unavailable source");
    drop(a);
    fs::remove_dir(&a_root).expect("remove owned empty mount replacement");
    fs::rename(&saved_root, &a_root).expect("reattach original selected source");
    a = start_node(&a_spec, &binaries).await;
    write(&a_root, "recovered.jpg", b"mount recovered\n");
    wait_file(
        &a_target.join("recovered.jpg"),
        b"mount recovered\n",
        "source resumes after its original mount returns",
    )
    .await;
    fs::remove_file(a_root.join("IMG_0001.jpg")).expect("delete only family A photo");
    write(
        &a_root,
        "delete-barrier.jpg",
        b"family A deletion observed\n",
    );
    wait_file(
        &a_target.join("delete-barrier.jpg"),
        b"family A deletion observed\n",
        "collection deletion barrier",
    )
    .await;
    wait_absent(
        &a_target.join("IMG_0001.jpg"),
        "family A deletion propagation",
    )
    .await;
    assert_eq!(
        fs::read(b_target.join("IMG_0001.jpg")).unwrap(),
        b"family B photo\n",
        "one link deleted another source's identically named photo"
    );

    // A missing folder marker must fail the mandatory scan before the source
    // can publish apparent deletions. Keep all removed data inside this fixture.
    a.stop().await.expect("stop source before scan failure");
    drop(a);
    let missing_marker = root.path().join("saved-source-marker");
    let missing_photo = root.path().join("saved-source-photo.jpg");
    fs::rename(a_root.join(".stfolder"), &missing_marker).expect("remove source marker");
    fs::rename(a_root.join("a-only.jpg"), &missing_photo)
        .expect("withhold indexed photo during failed scan");
    a = start_node(&a_spec, &binaries).await;
    wait_for_status(&a, "failed source scan remains blocked", |value| {
        value.get("lifecycle").and_then(Value::as_str) == Some("needsAttention")
            && value.get("issue").and_then(Value::as_str) == Some("initialScan")
    })
    .await;
    write(
        &b_root,
        "scan-failure-barrier.jpg",
        b"family B still works\n",
    );
    wait_file(
        &b_target.join("scan-failure-barrier.jpg"),
        b"family B still works\n",
        "healthy link during other source scan failure",
    )
    .await;
    assert_eq!(
        fs::read(a_target.join("a-only.jpg")).unwrap(),
        b"only family A\n",
        "failed source scan propagated a deletion"
    );
    a.stop().await.expect("stop source after scan failure");
    drop(a);
    fs::rename(&missing_marker, a_root.join(".stfolder")).expect("restore source marker");
    fs::rename(&missing_photo, a_root.join("a-only.jpg")).expect("restore indexed photo");
    a = start_node(&a_spec, &binaries).await;
    write(&a_root, "scan-recovered.jpg", b"scan recovered\n");
    wait_file(
        &a_target.join("scan-recovered.jpg"),
        b"scan recovered\n",
        "source recovers after successful scan",
    )
    .await;
    assert_absent(
        &b_root.join("scan-recovered.jpg"),
        "collection downloaded family A data to family B",
    );
    a.stop().await.expect("stop family A");
    b.stop().await.expect("stop family B");
    pool.stop().await.expect("stop collection");
}
