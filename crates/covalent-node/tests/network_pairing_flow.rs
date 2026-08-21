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
