//! End-to-end network pairing against two real nodes.
//!
//! Both client test suites assert their pairing paths against mocks, which is
//! exactly why `POST /api/v1/pair/network/start` shipped with no server route
//! at all. This test drives the documented discovery to pairing flow over the
//! real loopback HTTP API of two live [`NodeRuntime`] instances whose QUIC
//! endpoints talk to each other, so a missing route, a broken wire exchange, or
//! a short authentication string that does not agree across devices fails here.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use std::sync::Arc;

use covalent_core::{Engine, EngineOptions};
use covalent_node::advertised_address;
use covalent_node::network_pairing::{
    NetworkPairingManager, NetworkPairingWireOperation, NetworkPairingWireResponse,
};
use covalent_node::pairing_transport::PairingConnection;
use covalent_node::runtime::{NodeRuntime, NodeRuntimeConfig};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn loopback_zero() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

async fn start_node(directory: &TempDir, device_name: &str) -> NodeRuntime {
    let mut configuration =
        NodeRuntimeConfig::new(directory.path(), loopback_zero(), loopback_zero());
    configuration.device_name = device_name.to_owned();
    // A concrete advertised endpoint is what a discovered candidate carries and
    // what the signed transport binding must name.
    configuration.advertised_peer_address = Some(loopback_zero());
    NodeRuntime::start(configuration)
        .await
        .expect("start node runtime")
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
        .expect("write request");
    stream.flush().await.expect("flush request");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read HTTP response");
    let raw = String::from_utf8(raw).expect("UTF-8 HTTP response");
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response {raw:?}"));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("missing HTTP status in {head:?}"));
    HttpResponse {
        status,
        body: body.to_owned(),
    }
}

fn only_item(response: &HttpResponse) -> Value {
    let items = response.json();
    let items = items.as_array().expect("pairing list");
    assert_eq!(items.len(), 1, "expected exactly one pairing: {items:?}");
    items[0].clone()
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key} in {value:?}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_start_pending_and_confirm_pairs_two_real_nodes() {
    let initiator_data = TempDir::new().expect("initiator data");
    let responder_data = TempDir::new().expect("responder data");
    let initiator = start_node(&initiator_data, "Initiator laptop").await;
    let responder = start_node(&responder_data, "Responder server").await;

    // Discovery is the surface a client taps before pairing. It must answer for
    // an authenticated caller and yield a JSON candidate list.
    let discovery = call(&initiator, "GET", "/api/v1/discovery", None).await;
    assert_eq!(discovery.status, 200, "{}", discovery.body);
    assert!(discovery.json().is_array(), "{}", discovery.body);

    // A discovered candidate is an address plus port, exactly what the clients
    // post to start pairing.
    let candidate = responder.ready_info().peer_address();
    let start = call(
        &initiator,
        "POST",
        "/api/v1/pair/network/start",
        Some(&format!(r#"{{"candidateAddress":"{candidate}"}}"#)),
    )
    .await;
    assert_eq!(start.status, 200, "{}", start.body);
    let started = start.json();
    let pairing_id = field(&started, "pairingId").to_owned();
    let code = field(&started, "authenticationString").to_owned();
    assert_eq!(field(&started, "direction"), "outgoing");
    assert_eq!(field(&started, "state"), "awaiting_local_confirmation");
    assert_eq!(field(&started, "peerName"), "Responder server");
    assert!(started.get("peerTransport").is_none(), "{started:?}");

    // The responder learned of the same exchange over QUIC and derived the same
    // short authentication string. This is the property a human compares.
    let responder_pending = call(&responder, "GET", "/api/v1/pair/network/pending", None).await;
    assert_eq!(responder_pending.status, 200, "{}", responder_pending.body);
    let incoming = only_item(&responder_pending);
    assert_eq!(field(&incoming, "pairingId"), pairing_id);
    assert_eq!(field(&incoming, "authenticationString"), code);
    assert_eq!(field(&incoming, "direction"), "incoming");
    assert_eq!(field(&incoming, "state"), "awaiting_local_confirmation");
    assert_eq!(field(&incoming, "peerName"), "Initiator laptop");

    // A code that does not match the exchange is an identity failure, and it
    // must not advance either device.
    let wrong = call(
        &initiator,
        "POST",
        &format!("/api/v1/pair/network/{pairing_id}/confirm"),
        Some(r#"{"displayedCode":"0000-0000-0000-0000"}"#),
    )
    .await;
    assert_eq!(wrong.status, 403, "{}", wrong.body);
    let unchanged = only_item(&call(&initiator, "GET", "/api/v1/pair/network/pending", None).await);
    assert_eq!(field(&unchanged, "state"), "awaiting_local_confirmation");

    // First human confirms.
    let first = call(
        &initiator,
        "POST",
        &format!("/api/v1/pair/network/{pairing_id}/confirm"),
        Some(&format!(r#"{{"displayedCode":"{code}"}}"#)),
    )
    .await;
    assert_eq!(first.status, 200, "{}", first.body);
    assert_eq!(field(&first.json(), "state"), "awaiting_peer_confirmation");

    // Second human confirms, which finalizes the mutually signed exchange.
    let second = call(
        &responder,
        "POST",
        &format!("/api/v1/pair/network/{pairing_id}/confirm"),
        Some(&format!(r#"{{"displayedCode":"{code}"}}"#)),
    )
    .await;
    assert_eq!(second.status, 200, "{}", second.body);
    let completed = second.json();
    assert_eq!(field(&completed, "state"), "complete");
    let responder_view_of_initiator = completed
        .get("peerTransport")
        .unwrap_or_else(|| panic!("finalized pairing carries a peer transport: {completed:?}"));
    assert_eq!(
        field(responder_view_of_initiator, "address"),
        initiator.ready_info().peer_address().to_string()
    );

    // The initiator observes completion without any further local action.
    let initiator_final =
        only_item(&call(&initiator, "GET", "/api/v1/pair/network/pending", None).await);
    assert_eq!(field(&initiator_final, "state"), "complete");
    let initiator_view_of_responder = initiator_final.get("peerTransport").unwrap_or_else(|| {
        panic!("finalized pairing carries a peer transport: {initiator_final:?}")
    });
    assert_eq!(
        field(initiator_view_of_responder, "address"),
        candidate.to_string()
    );
    assert_eq!(
        field(initiator_view_of_responder, "displayName"),
        "Responder server"
    );

    // Both devices bound the same certificate, so each pinned the other exactly.
    assert_eq!(
        field(initiator_view_of_responder, "certificateFingerprint").len(),
        64
    );
    assert_ne!(
        field(initiator_view_of_responder, "certificateFingerprint"),
        field(responder_view_of_initiator, "certificateFingerprint")
    );

    // Mutual confirmation connects the peer as a pinned provider on both sides.
    for node in [&initiator, &responder] {
        let providers = call(node, "GET", "/api/v1/providers", None).await;
        assert_eq!(providers.status, 200, "{}", providers.body);
        let providers = providers.json();
        assert_eq!(providers.as_array().map(Vec::len), Some(1), "{providers:?}");
    }

    // Cancelling forgets the request on both devices.
    let cancelled = call(
        &initiator,
        "DELETE",
        &format!("/api/v1/pair/network/{pairing_id}"),
        None,
    )
    .await;
    assert_eq!(cancelled.status, 204, "{}", cancelled.body);
    let after_cancel = call(&initiator, "GET", "/api/v1/pair/network/pending", None).await;
    assert_eq!(after_cancel.json().as_array().map(Vec::len), Some(0));
    let peer_after_cancel = call(&responder, "GET", "/api/v1/pair/network/pending", None).await;
    assert_eq!(
        peer_after_cancel.json().as_array().map(Vec::len),
        Some(0),
        "cancelling must also forget the peer's retained request"
    );

    initiator.stop().await.expect("stop initiator");
    responder.stop().await.expect("stop responder");
}

#[tokio::test(flavor = "multi_thread")]
async fn starting_against_an_unreachable_candidate_reports_a_retryable_failure() {
    let data = TempDir::new().expect("data");
    let node = start_node(&data, "Lonely node").await;

    // A closed loopback port answers nothing; the route must exist and report a
    // retryable transport failure rather than 404 or a hang.
    let closed = tokio::net::UdpSocket::bind(loopback_zero())
        .await
        .expect("reserve a port");
    let address = closed.local_addr().expect("reserved address");
    drop(closed);

    let response = tokio::time::timeout(
        Duration::from_secs(30),
        call(
            &node,
            "POST",
            "/api/v1/pair/network/start",
            Some(&format!(r#"{{"candidateAddress":"{address}"}}"#)),
        ),
    )
    .await
    .expect("start must not hang");
    assert_eq!(response.status, 503, "{}", response.body);
    assert_eq!(
        response.json().get("code").and_then(Value::as_str),
        Some("pairing_peer_unreachable"),
        "{}",
        response.body
    );
    assert_eq!(
        call(&node, "GET", "/api/v1/pair/network/pending", None)
            .await
            .json()
            .as_array()
            .map(Vec::len),
        Some(0),
        "a failed dial must not strand a retained request"
    );

    node.stop().await.expect("stop node");
}

#[tokio::test(flavor = "multi_thread")]
async fn start_rejects_unknown_fields_and_public_routes() {
    let data = TempDir::new().expect("data");
    let node = start_node(&data, "Contract node").await;

    let unknown = call(
        &node,
        "POST",
        "/api/v1/pair/network/start",
        Some(r#"{"candidateAddress":"127.0.0.1:9","extra":true}"#),
    )
    .await;
    assert_eq!(unknown.status, 400, "{}", unknown.body);

    // A routable public address is refused before any packet leaves the device.
    let public = call(
        &node,
        "POST",
        "/api/v1/pair/network/start",
        Some(r#"{"candidateAddress":"93.184.216.34:8787"}"#),
    )
    .await;
    assert_eq!(public.status, 400, "{}", public.body);

    node.stop().await.expect("stop node");
}

/// Nonce slots one unauthenticated source burns before the legitimate pairing
/// below runs. Kept modest so the suite stays quick: the exhaustive proof that a
/// flood cannot exhaust the shared table lives in the unit tests beside the
/// admission policy, which exercise the real production constants. What this
/// test adds is that the property holds over a real QUIC endpoint, against a
/// live node, with source attribution taken from the actual connection.
const PAIRING_FLOOD_REQUESTS: usize = 96;

fn now_unix_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("millisecond clock fits u64")
}

/// An unrelated signing identity: a stranger on the LAN with a valid keypair,
/// which is all it takes to reach the responder's nonce table.
fn flooder(directory: &TempDir) -> NetworkPairingManager {
    let engine = Arc::new(Engine::open(EngineOptions::new(directory.path())).expect("engine"));
    NetworkPairingManager::open(engine, directory.path().join("network-pairing.json"))
        .expect("flooder pairing manager")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pairing_flood_cannot_deny_pairing_or_weaken_replay_and_skew_checks() {
    let initiator_data = TempDir::new().expect("initiator data");
    let responder_data = TempDir::new().expect("responder data");
    let flooder_data = TempDir::new().expect("flooder data");
    let initiator = start_node(&initiator_data, "Initiator laptop").await;
    let responder = start_node(&responder_data, "Responder server").await;
    let attacker = flooder(&flooder_data);
    let target = responder.ready_info().peer_address();

    // Burn nonce slots on the responder from one source, over the same
    // pairing-only ALPN a real peer uses. Every one of these is a fresh, valid,
    // correctly signed request, so each reaches the nonce table.
    let connection = PairingConnection::connect(target)
        .await
        .expect("dial the pairing ALPN");
    for index in 0..PAIRING_FLOOD_REQUESTS {
        let request = attacker
            .sign_wire_request(NetworkPairingWireOperation::Probe, now_unix_ms())
            .expect("sign flood probe");
        let response = connection.request(&request).await.expect("flood probe");
        assert!(
            matches!(response, NetworkPairingWireResponse::Probe { .. }),
            "a source inside its budget must be served, not refused (request {index})"
        );
    }

    // Replay protection survives the rate limiting: re-sending a request the
    // responder already consumed is still refused. If budgets had been allowed
    // to scope the uniqueness lookup, this is where the hole would open.
    let consumed = attacker
        .sign_wire_request(NetworkPairingWireOperation::Probe, now_unix_ms())
        .expect("sign replay probe");
    assert!(
        matches!(
            connection.request(&consumed).await.expect("first send"),
            NetworkPairingWireResponse::Probe { .. }
        ),
        "the first use of a fresh nonce is accepted"
    );
    match connection.request(&consumed).await.expect("replayed send") {
        NetworkPairingWireResponse::Failed { code, .. } => {
            assert_eq!(code, "pairing_request_rejected", "replay must be refused");
        }
        other => panic!(
            "a replayed nonce must not be accepted: {}",
            serde_json::to_string(&other).unwrap_or_default()
        ),
    }

    // Skew enforcement survives it too: a correctly signed request issued well
    // outside the accepted window is refused before it can take a nonce slot.
    let stale = attacker
        .sign_wire_request(
            NetworkPairingWireOperation::Probe,
            now_unix_ms().saturating_sub(10 * 60 * 1_000),
        )
        .expect("sign stale probe");
    match connection.request(&stale).await.expect("stale send") {
        NetworkPairingWireResponse::Failed { code, .. } => {
            assert_eq!(
                code, "pairing_request_rejected",
                "a request outside the skew window must be refused"
            );
        }
        other => panic!(
            "a stale request must not be accepted: {}",
            serde_json::to_string(&other).unwrap_or_default()
        ),
    }
    drop(connection);

    // With the flood's slots still held, a real pairing between two real nodes
    // completes end to end. This is the onboarding flow the denial-of-service
    // took out.
    let start = call(
        &initiator,
        "POST",
        "/api/v1/pair/network/start",
        Some(&format!(r#"{{"candidateAddress":"{target}"}}"#)),
    )
    .await;
    assert_eq!(start.status, 200, "{}", start.body);
    let started = start.json();
    let pairing_id = field(&started, "pairingId").to_owned();
    let code = field(&started, "authenticationString").to_owned();

    let incoming = only_item(&call(&responder, "GET", "/api/v1/pair/network/pending", None).await);
    assert_eq!(field(&incoming, "pairingId"), pairing_id);
    assert_eq!(
        field(&incoming, "authenticationString"),
        code,
        "both devices must still derive the same short authentication string"
    );

    let first = call(
        &initiator,
        "POST",
        &format!("/api/v1/pair/network/{pairing_id}/confirm"),
        Some(&format!(r#"{{"displayedCode":"{code}"}}"#)),
    )
    .await;
    assert_eq!(first.status, 200, "{}", first.body);
    let second = call(
        &responder,
        "POST",
        &format!("/api/v1/pair/network/{pairing_id}/confirm"),
        Some(&format!(r#"{{"displayedCode":"{code}"}}"#)),
    )
    .await;
    assert_eq!(second.status, 200, "{}", second.body);
    assert_eq!(
        field(&second.json(), "state"),
        "complete",
        "a flood must not deny a legitimate pairing"
    );
}

/// Identity of a committed pairing state file, or `None` while it does not exist.
///
/// The node stages every durable commit into a fresh temporary file and renames
/// it over the target, so each commit installs a new inode. Watching the inode
/// therefore counts commits as the filesystem saw them, which no counter kept by
/// the code under test could honestly do.
fn state_file_identity(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.ino(), metadata.len()))
}

/// Wire requests used to cost one full state-file rewrite and two fsyncs each,
/// unauthenticated and unbounded. This drives real requests over real QUIC at a
/// live node and watches its actual state file, then proves the writes that were
/// removed were not the ones a crash needs.
#[tokio::test(flavor = "multi_thread")]
async fn wire_requests_never_commit_the_state_file_but_consent_survives_a_restart() {
    let initiator_data = TempDir::new().expect("initiator data");
    let responder_data = TempDir::new().expect("responder data");
    let flooder_data = TempDir::new().expect("flooder data");
    let initiator = start_node(&initiator_data, "Initiator laptop").await;
    let responder = start_node(&responder_data, "Responder server").await;
    let attacker = flooder(&flooder_data);
    let target = responder.ready_info().peer_address();
    let state_path = responder_data.path().join("network-pairing.json");

    // Starting the node committed the replay floor. That is the one durable
    // write a process start is allowed, and everything below is measured
    // against it.
    let at_start =
        state_file_identity(&state_path).expect("the replay floor is committed at start");

    let connection = PairingConnection::connect(target)
        .await
        .expect("dial the pairing ALPN");
    for index in 0..PAIRING_FLOOD_REQUESTS {
        let request = attacker
            .sign_wire_request(NetworkPairingWireOperation::Probe, now_unix_ms())
            .expect("sign probe");
        let response = connection.request(&request).await.expect("probe");
        assert!(
            matches!(response, NetworkPairingWireResponse::Probe { .. }),
            "a source inside its budget must still be served (request {index})"
        );
    }
    drop(connection);

    assert_eq!(
        state_file_identity(&state_path),
        Some(at_start),
        "{PAIRING_FLOOD_REQUESTS} accepted wire requests must not commit the durable state file once"
    );

    // What was removed is write amplification, not durability. A pairing that a
    // human actually confirmed still commits, and still survives a restart.
    let start = call(
        &initiator,
        "POST",
        "/api/v1/pair/network/start",
        Some(&format!(r#"{{"candidateAddress":"{target}"}}"#)),
    )
    .await;
    assert_eq!(start.status, 200, "{}", start.body);
    let started = start.json();
    let pairing_id = field(&started, "pairingId").to_owned();
    let code = field(&started, "authenticationString").to_owned();

    for node in [&initiator, &responder] {
        let confirmed = call(
            node,
            "POST",
            &format!("/api/v1/pair/network/{pairing_id}/confirm"),
            Some(&format!(r#"{{"displayedCode":"{code}"}}"#)),
        )
        .await;
        assert_eq!(confirmed.status, 200, "{}", confirmed.body);
    }

    let after_consent =
        state_file_identity(&state_path).expect("consent leaves a committed state file");
    assert_ne!(
        after_consent, at_start,
        "a human comparing a code and pressing confirm must reach the disk"
    );

    // Restart the responder over the same data directory. The mutually
    // confirmed exchange has to still be there: nothing can rebuild it.
    responder.stop().await.expect("stop responder");
    let restarted = start_node(&responder_data, "Responder server").await;
    let retained = only_item(&call(&restarted, "GET", "/api/v1/pair/network/pending", None).await);
    assert_eq!(field(&retained, "pairingId"), pairing_id);
    assert_eq!(
        field(&retained, "state"),
        "complete",
        "mutual consent must survive a restart"
    );

    restarted.stop().await.expect("stop restarted responder");
    initiator.stop().await.expect("stop initiator");
}

/// Builds the `Submit` an attacker uses to reach the probe.
///
/// It asks the victim for an invitation naming a transport binding of its own
/// choosing, accepts that invitation locally, and signs the resulting exchange
/// back. `claimed_address` is the address the victim would dial — the parameter
/// this whole defect is about.
async fn signed_submit_naming(
    connection: &PairingConnection,
    attacker: &NetworkPairingManager,
    victim: SocketAddr,
    claimed_address: SocketAddr,
) -> covalent_node::network_pairing::NetworkPairingWireRequest {
    let binding = attacker
        .local_transport_binding(claimed_address, b"attacker certificate bytes")
        .expect("attacker transport binding");
    let start = attacker
        .sign_wire_request(
            NetworkPairingWireOperation::Start {
                responder_transport: binding.clone(),
            },
            now_unix_ms(),
        )
        .expect("sign start");
    let NetworkPairingWireResponse::Invitation { invitation } =
        connection.request(&start).await.expect("start")
    else {
        panic!("the victim must issue an invitation for a well formed start");
    };
    let session = attacker
        .register_outgoing(
            victim,
            connection.observed_certificate(),
            *invitation,
            binding,
            now_unix_ms(),
        )
        .expect("attacker session");
    attacker
        .sign_wire_request(
            NetworkPairingWireOperation::Submit {
                pairing_id: session.invitation().invitation_id.clone(),
                session: Box::new(session),
            },
            now_unix_ms(),
        )
        .expect("sign submit")
}

fn failure_code(response: &NetworkPairingWireResponse) -> String {
    match response {
        NetworkPairingWireResponse::Failed { code, .. } => code.clone(),
        other => panic!(
            "expected a stable wire failure: {}",
            serde_json::to_string(other).unwrap_or_default()
        ),
    }
}

/// `Submit` is the one path that makes this node dial an address a stranger
/// named. This drives that path from a live attacker over real QUIC and pins
/// down exactly how much of it survives: not a LAN scanner, and not a free one.
#[tokio::test(flavor = "multi_thread")]
async fn submit_cannot_aim_this_node_at_a_third_host_and_probes_are_rationed() {
    let initiator_data = TempDir::new().expect("initiator data");
    let responder_data = TempDir::new().expect("responder data");
    let attacker_data = TempDir::new().expect("attacker data");
    let initiator = start_node(&initiator_data, "Initiator laptop").await;
    let victim = start_node(&responder_data, "Responder server").await;
    let attacker = flooder(&attacker_data);
    let target = victim.ready_info().peer_address();

    // A real pairing first, so the budget is proven to accommodate the traffic
    // it exists to protect before any of it is spent on an attack.
    let start = call(
        &initiator,
        "POST",
        "/api/v1/pair/network/start",
        Some(&format!(r#"{{"candidateAddress":"{target}"}}"#)),
    )
    .await;
    assert_eq!(
        start.status, 200,
        "the probe guard must not break a legitimate pairing: {}",
        start.body
    );
    let started = start.json();
    let pairing_id = field(&started, "pairingId").to_owned();
    let code = field(&started, "authenticationString").to_owned();
    for node in [&initiator, &victim] {
        let confirmed = call(
            node,
            "POST",
            &format!("/api/v1/pair/network/{pairing_id}/confirm"),
            Some(&format!(r#"{{"displayedCode":"{code}"}}"#)),
        )
        .await;
        assert_eq!(confirmed.status, 200, "{}", confirmed.body);
    }
    assert_eq!(
        field(
            &only_item(&call(&victim, "GET", "/api/v1/pair/network/pending", None).await),
            "state"
        ),
        "complete",
        "a legitimate pairing must still complete end to end"
    );

    let connection = PairingConnection::connect(target)
        .await
        .expect("dial the pairing ALPN");

    // A third host on the LAN. The route check alone would wave this through —
    // it is a private address — so this is precisely the reflection primitive.
    // It must be refused, and refused without a packet leaving the victim.
    let reflection = signed_submit_naming(
        &connection,
        &attacker,
        target,
        "192.168.1.5:8787".parse().expect("third host"),
    )
    .await;
    let started_at = std::time::Instant::now();
    let refused = connection.request(&reflection).await.expect("reflection");
    let refusal_took = started_at.elapsed();
    assert_eq!(
        failure_code(&refused),
        "pairing_identity_mismatch",
        "a submit must not steer this node at a host it did not arrive from"
    );
    assert!(
        refusal_took < Duration::from_secs(1),
        "refusing a reflection must cost no dial at all, took {refusal_took:?}"
    );

    // The attacker's own address is all it has left, and only the port varies.
    // The first attempt is allowed to dial; the second at the same address must
    // be served from the remembered failure instead of dialing again.
    let closed = |port: u16| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let first = signed_submit_naming(&connection, &attacker, target, closed(1)).await;
    let first_response = connection.request(&first).await.expect("first probe");
    assert!(
        matches!(first_response, NetworkPairingWireResponse::Failed { .. }),
        "a probe of a closed port cannot produce a pairing"
    );

    let repeat = signed_submit_naming(&connection, &attacker, target, closed(1)).await;
    let started_at = std::time::Instant::now();
    let cached = connection.request(&repeat).await.expect("repeat probe");
    let cached_took = started_at.elapsed();
    assert_eq!(
        failure_code(&cached),
        "pairing_unavailable",
        "a repeated probe of a known-dead address must be answered from memory"
    );
    assert!(
        cached_took < Duration::from_secs(1),
        "a cached probe failure must not redial, took {cached_took:?}"
    );

    // Fresh addresses each cost a slot, and the budget runs out. Scanning ports
    // is what is left of the primitive, and this is the rate it is left at.
    let mut refusals = Vec::new();
    for port in 2..=8_u16 {
        let attempt = signed_submit_naming(&connection, &attacker, target, closed(port)).await;
        refusals.push(failure_code(
            &connection.request(&attempt).await.expect("budgeted probe"),
        ));
    }
    assert!(
        refusals.contains(&"pairing_resource_limit".to_owned()),
        "one source must run out of probe budget rather than scan freely: {refusals:?}"
    );
    assert_eq!(
        refusals.last().map(String::as_str),
        Some("pairing_resource_limit"),
        "the budget must stay spent for the rest of the window: {refusals:?}"
    );

    drop(connection);
    victim.stop().await.expect("stop victim");
    initiator.stop().await.expect("stop initiator");
}

/// Starts a node exactly the way a real deployment starts one.
///
/// The difference from [`start_node`] is the entire point of this test, and it
/// is one line: nothing here assigns `advertised_peer_address`. That field was
/// set in precisely one place in the whole repository — `start_node` above — so
/// every test passed while `AppState::peer_address` was `None` on Unraid,
/// macOS, Android and the web console alike. `GET /api/v1/transport/identity`
/// and `GET /api/v1/discovery` answered 500 and
/// `POST /api/v1/pair/invitations` answered 400 `invalid_contract` on every
/// real install, and no test in the tree could observe it, because the harness
/// supplied the one value production never did.
///
/// So this test asserts the production configuration path rather than a
/// convenient one. It cannot assert a fixed address — a CI runner's interfaces
/// are not knowable in advance, and inventing a way to inject them would
/// reintroduce exactly the blind spot being closed. It asserts the properties
/// that hold on every host instead:
///
/// * no route reports missing configuration as an internal fault, or as a
///   malformed request;
/// * the three routes agree with each other, so a node cannot advertise itself
///   as pairable through one route and unpairable through another;
/// * when an endpoint is resolved it is concrete and dialable, never loopback
///   or unspecified — an address a peer cannot use is worse than none.
///
/// Address selection itself is exhaustively covered as pure arithmetic in
/// `covalent_node::advertised_address`, including the container-bridge case
/// this host may or may not be in.
#[tokio::test]
async fn a_node_started_the_production_way_resolves_or_refuses_coherently() {
    let directory = TempDir::new().expect("temp directory");
    let configuration = NodeRuntimeConfig::new(directory.path(), loopback_zero(), loopback_zero());
    let node = NodeRuntime::start(configuration)
        .await
        .expect("a node with no advertised address configured must still start");

    let identity = call(&node, "GET", "/api/v1/transport/identity", None).await;
    let discovery = call(&node, "GET", "/api/v1/discovery", None).await;
    let invitation = call(
        &node,
        "POST",
        "/api/v1/pair/invitations",
        Some(r#"{"lifetimeMs":600000}"#),
    )
    .await;

    for (label, response) in [
        ("transport/identity", &identity),
        ("discovery", &discovery),
        ("pair/invitations", &invitation),
    ] {
        assert_ne!(
            response.status, 500,
            "{label} must never report configuration state as an internal fault"
        );
        if response.status != 200 {
            let body = response.json();
            let code = body["code"].as_str().unwrap_or_default().to_owned();
            assert_eq!(
                code, "peer_endpoint_unavailable",
                "{label} answered {} with code {code}; a well-formed request against a \
                 healthy node must not be called malformed",
                response.status
            );
            assert_eq!(response.status, 409, "{label} status");
        }
    }

    // The load-bearing assertion, and the one that actually fails against the
    // old runtime. Everything above holds even with address resolution removed,
    // because the error taxonomy alone guarantees it; asserting only that would
    // be a test that looks strict and catches nothing. So compute what this host
    // *should* resolve to and require the running node to agree. On a machine
    // with a usable private address the node must have one, and 409 is a
    // failure; on a bridged container with nothing dialable, 409 is required and
    // success would mean the node is advertising a dead end.
    //
    // Selection itself is verified independently as pure arithmetic in
    // `advertised_address`; what is under test here is that the production
    // startup path consults it at all, which is precisely what it did not do.
    let expected = advertised_address::select_advertised_address(
        &advertised_address::observed_interface_addresses(),
        advertised_address::running_in_container(),
    );
    assert_eq!(
        identity.status == 200,
        expected.is_ok(),
        "this host resolves {expected:?}, so the node's answer of {} contradicts it",
        identity.status
    );

    assert_eq!(
        identity.status == 200,
        discovery.status == 200,
        "discovery and transport identity must agree about whether this node is reachable"
    );
    assert_eq!(
        identity.status == 200,
        invitation.status == 200,
        "invitations and transport identity must agree about whether this node is reachable"
    );

    if invitation.status == 200 {
        let body = invitation.json();
        let address = body["transportBinding"]["address"]
            .as_str()
            .expect("a served invitation names the address peers dial")
            .to_owned();
        let parsed: SocketAddr = address.parse().expect("advertised address is concrete");
        assert!(
            !parsed.ip().is_unspecified() && parsed.port() != 0,
            "an advertised endpoint must be dialable, got {address}"
        );
        assert!(
            !parsed.ip().is_loopback(),
            "auto-detection must never hand peers a loopback address, got {address}"
        );
        assert_eq!(
            parsed.ip(),
            expected.expect("a served invitation implies an address was resolvable"),
            "the node must advertise the address selection chose, not some other interface"
        );
    }

    node.stop().await.expect("stop node");
}
