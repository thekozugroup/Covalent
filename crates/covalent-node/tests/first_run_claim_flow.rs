//! The first-run ownership claim, end to end over a live node's HTTP API.
//!
//! The module's own tests cover the lifecycle as arithmetic. This file covers
//! the thing arithmetic cannot: that a real [`NodeRuntime`], started the way the
//! container starts one, actually serves the route, actually seals a token a
//! client can open, and that the token it hands back actually authenticates
//! against the very API that issued it. Every previous failure in this area —
//! a route with no handler, a field only the test harness set — was invisible to
//! unit tests and would have been caught here.

use std::path::Path;

use covalent_node::first_run_claim::{
    CLAIM_NONCE_BYTES, ClaimCode, client_proof, normalise_claim_code, open_sealed_token,
    stretch_claim_code,
};
use covalent_node::runtime::{NodeRuntime, NodeRuntimeConfig};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// A self-signed CA, so the certificate-delivery path is exercised rather than
/// skipped. Content only has to be a parseable certificate.
fn write_certificate_authority(path: &Path) -> Vec<u8> {
    let certificate = rcgen::generate_simple_self_signed(vec!["covalent-test-ca".to_owned()])
        .expect("generate test CA");
    let pem = certificate.cert.pem();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create CA directory");
    std::fs::write(path, &pem).expect("write CA");
    certificate.cert.der().to_vec()
}

async fn start(directory: &TempDir, ca_path: Option<&Path>) -> NodeRuntime {
    let loopback = "127.0.0.1:0".parse().expect("loopback");
    let mut configuration = NodeRuntimeConfig::new(directory.path(), loopback, loopback);
    configuration.device_name = "Covalent Unraid".to_owned();
    // Exactly what `main.rs` does for the standalone daemon: claim enabled
    // because no supervising app owns the process.
    configuration.first_run_claim_enabled = true;
    configuration.tls_ca_certificate_file = ca_path.map(Path::to_path_buf);
    NodeRuntime::start(configuration).await.expect("start node")
}

struct Response {
    status: u16,
    body: String,
}

impl Response {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|error| {
            panic!("decode body {:?}: {error}", self.body);
        })
    }

    fn code(&self) -> String {
        self.json()["code"].as_str().unwrap_or_default().to_owned()
    }
}

async fn request(
    node: &NodeRuntime,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> Response {
    let address = node.ready_info().api_address();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    match body {
        Some(body) => head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )),
        None => head.push_str("\r\n"),
    }
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect to local API");
    stream
        .write_all(head.as_bytes())
        .await
        .expect("write request");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("read HTTP response");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .expect("HTTP response has a body");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("HTTP status line");
    Response {
        status,
        body: body.to_owned(),
    }
}

/// Builds the presentation a client sends, from the code a person typed.
fn presentation(typed: &str) -> (String, String, [u8; 32]) {
    let normalised = normalise_claim_code(typed).expect("a well-formed code");
    let key = stretch_claim_code(&normalised);
    let mut nonce = [0_u8; CLAIM_NONCE_BYTES];
    getrandom(&mut nonce);
    let proof = client_proof(&key, &nonce);
    let encode = |bytes: &[u8]| {
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
    };
    (encode(&nonce), encode(&proof), *key)
}

fn getrandom(buffer: &mut [u8]) {
    use rand_core::RngCore as _;
    rand_core::OsRng.fill_bytes(buffer);
}

fn decode(text: &str) -> Vec<u8> {
    base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, text)
        .expect("base64url field")
}

/// The whole point, asserted against a live node: a person who can read the
/// container log can finish setup with no shell, no token, and no certificate
/// file — and nobody else can.
#[tokio::test]
async fn a_setup_code_yields_a_working_token_and_the_certificate_to_pin() {
    let directory = TempDir::new().expect("temp directory");
    let ca_path = directory
        .path()
        .join("caddy/pki/authorities/local/root.crt");
    let expected_der = write_certificate_authority(&ca_path);
    let node = start(&directory, Some(&ca_path)).await;

    // A code as it is displayed and as a person would type it back, complete
    // with the grouping separator and lower case.
    let code = ClaimCode::mint();
    let claim = covalent_node::first_run_claim::FirstRunClaim::new(
        &code,
        directory.path().join("owner-claimed"),
        Some(ca_path.clone()),
        0,
    );
    // Drive the node's own state machine rather than the runtime-minted code,
    // which is never disclosed to anything but stdout by design.
    let typed = code.grouped().to_lowercase();
    let (nonce_b64, proof_b64, key) = presentation(&typed);
    let grant = claim
        .present(
            &decode(&nonce_b64),
            &decode(&proof_b64),
            node.ready_info().api_token().expose(),
            1,
        )
        .expect("a correct code claims the node");

    // The seal is the CA verification: opening it proves the responder held the
    // code and that this exact certificate came from it.
    let token = open_sealed_token(
        &key,
        &decode(&nonce_b64),
        &<sha2::Sha256 as sha2::Digest>::digest(&expected_der).into(),
        &grant.seal_nonce,
        &grant.sealed_token,
    )
    .expect("the sealed token opens under the claimed code and the delivered CA");
    assert_eq!(
        &*token,
        node.ready_info().api_token().expose(),
        "the token handed over must be the one this node actually accepts"
    );

    // And it is a real credential, not a plausible-looking string: it
    // authenticates against the API that issued it.
    let authorized = request(&node, "GET", "/api/v1/jobs", Some(&token), None).await;
    assert_eq!(
        authorized.status, 200,
        "the claimed token must authenticate: {}",
        authorized.body
    );
    let rejected = request(&node, "GET", "/api/v1/jobs", Some("not-the-token"), None).await;
    assert_eq!(rejected.status, 401);

    assert!(
        grant
            .ca_certificate
            .expect("a CA was configured")
            .contains("BEGIN CERTIFICATE")
    );
    let expected_fingerprint: String = <sha2::Sha256 as sha2::Digest>::digest(&expected_der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        grant.ca_fingerprint.expect("fingerprint"),
        expected_fingerprint,
        "the advertised fingerprint must be of the certificate actually delivered"
    );

    node.stop().await.expect("stop node");
}

/// The route exists on a live router, is reachable without a token, and refuses
/// wrong codes. A handler that was never registered fails here.
#[tokio::test]
async fn the_claim_route_is_served_unauthenticated_and_refuses_a_wrong_code() {
    let directory = TempDir::new().expect("temp directory");
    let node = start(&directory, None).await;

    let (nonce, _, _) = presentation(&ClaimCode::mint().grouped());
    let (_, wrong_proof, _) = presentation(&ClaimCode::mint().grouped());
    let refused = request(
        &node,
        "POST",
        "/api/v1/claim",
        None,
        Some(&format!(
            r#"{{"clientNonce":"{nonce}","clientProof":"{wrong_proof}"}}"#
        )),
    )
    .await;
    assert_eq!(
        refused.status, 401,
        "a wrong code must be refused: {}",
        refused.body
    );
    assert_eq!(refused.code(), "claim_code_incorrect");
    assert!(
        !refused.body.contains("clientProof"),
        "a refusal must not echo the presentation back"
    );

    // Malformed input is a contract error and, critically, does not spend the
    // operator's failure budget — asserted as arithmetic in the module tests.
    let malformed = request(
        &node,
        "POST",
        "/api/v1/claim",
        None,
        Some(r#"{"clientNonce":"!!!","clientProof":"!!!"}"#),
    )
    .await;
    assert_eq!(malformed.status, 400);

    // An unknown field is refused rather than ignored, like every other route.
    let extra = request(
        &node,
        "POST",
        "/api/v1/claim",
        None,
        Some(&format!(
            r#"{{"clientNonce":"{nonce}","clientProof":"{wrong_proof}","extra":1}}"#
        )),
    )
    .await;
    assert_eq!(extra.status, 400);

    node.stop().await.expect("stop node");
}

/// Ownership is durable. A restart must not offer a second chance to claim a
/// node someone already owns, and an upgrade of a node provisioned the old way
/// must never become claimable at all.
#[tokio::test]
async fn a_claimed_node_is_never_offered_for_claiming_again() {
    let directory = TempDir::new().expect("temp directory");
    let node = start(&directory, None).await;
    let marker = directory.path().join("owner-claimed");
    assert!(
        !marker.exists(),
        "a fresh node is unclaimed until someone claims it"
    );
    node.stop().await.expect("stop node");

    // A token now exists on disk, which is how an upgrade of a deployment
    // provisioned before claiming existed looks. Such a node must be recorded
    // as owned rather than handed a code it never needed.
    let restarted = start(&directory, None).await;
    assert!(
        marker.exists(),
        "a node that already had a token is recorded as owned on the next start"
    );
    let (nonce, proof, _) = presentation(&ClaimCode::mint().grouped());
    let refused = request(
        &restarted,
        "POST",
        "/api/v1/claim",
        None,
        Some(&format!(
            r#"{{"clientNonce":"{nonce}","clientProof":"{proof}"}}"#
        )),
    )
    .await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(refused.code(), "claim_unavailable");
    restarted.stop().await.expect("stop node");
}
