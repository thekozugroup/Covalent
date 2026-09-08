use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixListener;

const KEY: &str = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

struct Server {
    _directory: TempDir,
    listener: UnixListener,
    client: EngineApiClient,
}

impl Server {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("cvs-api-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::fs::canonicalize(directory.path())
            .unwrap()
            .join("api.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let client = EngineApiClient::new(path, Zeroizing::new(KEY.to_owned())).unwrap();
        Self {
            _directory: directory,
            listener,
            client,
        }
    }

    async fn accept_request(&self) -> (UnixStream, Vec<u8>) {
        let (mut socket, _) = self.listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut part = [0; 2048];
        loop {
            let count = socket.read(&mut part).await.unwrap();
            assert!(count != 0, "request ended before headers");
            bytes.extend_from_slice(&part[..count]);
            assert!(bytes.len() <= MAX_CONFIGURATION_BYTES + 16 * 1024);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        (socket, bytes)
    }

    async fn respond(&self, response: &[u8]) {
        let (mut socket, _) = self.accept_request().await;
        // A client correctly rejecting an oversized message may close early.
        let _ = socket.write_all(response).await;
        let _ = socket.shutdown().await;
    }
}

#[tokio::test]
async fn sends_secret_only_in_sensitive_header_and_decodes_json() {
    let server = Server::new();
    assert_eq!(format!("{:?}", server.client), "EngineApiClient([PRIVATE])");
    let server_task = async {
        let (mut socket, request) = server.accept_request().await;
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /rest/system/status HTTP/1.1\r\n"));
        assert!(request.contains(&format!("x-api-key: {KEY}\r\n")));
        assert_eq!(request.matches(KEY).count(), 1);
        assert!(request.contains("host: localhost\r\n"));
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}").await.unwrap();
    };
    let (result, ()) = tokio::join!(
        server
            .client
            .json::<serde_json::Value>(EngineEndpoint::SystemStatus),
        server_task
    );
    assert_eq!(result.unwrap(), serde_json::json!({"ok": true}));
}

#[tokio::test]
async fn status_errors_and_redirects_do_not_disclose_or_follow_server_content() {
    for status in [301, 302, 401, 403, 500] {
        let server = Server::new();
        let response = format!(
            "HTTP/1.1 {status} Rejected\r\nLocation: http://127.0.0.1:1/{KEY}\r\nContent-Length: 64\r\n\r\n{KEY}"
        );
        let (result, ()) = tokio::join!(
            server
                .client
                .json::<serde_json::Value>(EngineEndpoint::SystemStatus),
            server.respond(response.as_bytes())
        );
        let error = result.unwrap_err();
        assert_eq!(error, EngineApiError::HttpStatus(status));
        assert!(!format!("{error} {error:?}").contains(KEY));
    }
}

#[tokio::test]
async fn rejects_declared_and_streamed_response_overflow() {
    let server = Server::new();
    let declared = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_RESPONSE_BYTES + 1
    );
    let (result, ()) = tokio::join!(
        server
            .client
            .json::<serde_json::Value>(EngineEndpoint::SystemStatus),
        server.respond(declared.as_bytes())
    );
    assert_eq!(result.unwrap_err(), EngineApiError::BodyTooLarge);

    let server = Server::new();
    let mut streamed = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    streamed.extend_from_slice(format!("{:x}\r\n", MAX_RESPONSE_BYTES + 1).as_bytes());
    streamed.resize(streamed.len() + MAX_RESPONSE_BYTES + 1, b'a');
    streamed.extend_from_slice(b"\r\n0\r\n\r\n");
    let (result, ()) = tokio::join!(
        server
            .client
            .json::<serde_json::Value>(EngineEndpoint::SystemStatus),
        server.respond(&streamed)
    );
    assert_eq!(result.unwrap_err(), EngineApiError::BodyTooLarge);
}

#[tokio::test]
async fn rejects_truncated_compressed_malformed_and_deep_json() {
    let cases = [
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 40\r\n\r\n{}".to_owned(),
            EngineApiError::Unavailable,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n\r\n{}".to_owned(),
            EngineApiError::InvalidResponse,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nwrong".to_owned(),
            EngineApiError::InvalidResponse,
        ),
        (
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: 400\r\n\r\n{}{}",
                "[".repeat(200),
                "]".repeat(200)
            ),
            EngineApiError::InvalidResponse,
        ),
    ];
    for (response, expected) in cases {
        let server = Server::new();
        let (result, ()) = tokio::join!(
            server
                .client
                .json::<serde_json::Value>(EngineEndpoint::SystemStatus),
            server.respond(response.as_bytes())
        );
        assert_eq!(result.unwrap_err(), expected);
    }
}

#[tokio::test]
async fn whole_exchange_deadline_closes_a_stalled_response() {
    let mut server = Server::new();
    server.client.timeout = Duration::from_millis(80);
    let peer = async {
        let (mut socket, _) = server.accept_request().await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\n{")
            .await
            .unwrap();
        let mut byte = [0];
        let count = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 0, "timed-out client retained its connection");
    };
    let (result, ()) = tokio::join!(
        server
            .client
            .json::<serde_json::Value>(EngineEndpoint::SystemStatus),
        peer
    );
    assert_eq!(result.unwrap_err(), EngineApiError::Timeout);
}

#[tokio::test]
async fn cancellation_drops_the_connection_driver() {
    let server = Server::new();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let peer = async {
        let (mut socket, _) = server.accept_request().await;
        ready_tx.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    };
    let caller = async {
        let request = server
            .client
            .json::<serde_json::Value>(EngineEndpoint::SystemStatus);
        tokio::pin!(request);
        tokio::select! {
            result = &mut request => panic!("request unexpectedly ended: {result:?}"),
            () = async { ready_rx.await.unwrap() } => {},
        }
    };
    tokio::join!(caller, peer);
}

#[tokio::test]
async fn checks_private_socket_and_directory_again_for_each_request() {
    let server = Server::new();
    std::fs::set_permissions(
        &server.client.socket_path,
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert_eq!(
        server
            .client
            .json::<serde_json::Value>(EngineEndpoint::SystemStatus)
            .await
            .unwrap_err(),
        EngineApiError::UnsafeEndpoint
    );
    std::fs::set_permissions(
        &server.client.socket_path,
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::set_permissions(
        server._directory.path(),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(
        server
            .client
            .json::<serde_json::Value>(EngineEndpoint::SystemStatus)
            .await
            .unwrap_err(),
        EngineApiError::UnsafeEndpoint
    );
    std::fs::set_permissions(
        server._directory.path(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
}

#[tokio::test]
async fn rejects_socket_symlink_without_connecting() {
    let server = Server::new();
    let actual = server._directory.path().join("actual.sock");
    std::fs::rename(&server.client.socket_path, &actual).unwrap();
    symlink(&actual, &server.client.socket_path).unwrap();
    assert_eq!(
        server
            .client
            .json::<serde_json::Value>(EngineEndpoint::SystemStatus)
            .await
            .unwrap_err(),
        EngineApiError::UnsafeEndpoint
    );
}

#[tokio::test]
async fn rejects_oversized_config_before_connecting() {
    let server = Server::new();
    let configuration = serde_json::json!({"large": "a".repeat(MAX_CONFIGURATION_BYTES)});
    assert_eq!(
        server
            .client
            .replace_configuration(&configuration)
            .await
            .unwrap_err(),
        EngineApiError::BodyTooLarge
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), server.listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn rejects_nonlocal_path_forms_and_header_injection() {
    for path in [
        "relative.sock".to_owned(),
        "/tmp/../api.sock".to_owned(),
        format!("/{}", "a".repeat(101)),
    ] {
        assert_eq!(
            EngineApiClient::new(path.into(), Zeroizing::new(KEY.to_owned())).unwrap_err(),
            EngineApiError::InvalidConfiguration
        );
    }
    for key in [
        "x".repeat(64),
        format!("{}\r\n", &KEY[..62]),
        "a".repeat(63),
    ] {
        assert_eq!(
            EngineApiClient::new("/tmp/api.sock".into(), Zeroizing::new(key)).unwrap_err(),
            EngineApiError::InvalidConfiguration
        );
    }
    let id = Uuid::new_v4();
    assert_eq!(
        EngineEndpoint::FolderStatus(id).path(),
        format!("/rest/db/status?folder={id}")
    );
    assert_eq!(
        EngineEndpoint::FolderErrors(id).path(),
        format!("/rest/folder/errors?folder={id}&page=1&perpage=128")
    );
    assert_eq!(
        EngineEndpoint::FolderVersions(id).path(),
        format!("/rest/folder/versions?folder={id}")
    );
}

#[test]
fn folder_endpoints_use_canonical_uuid_query_values_only() {
    let id = Uuid::parse_str("52f06d0f-8ff6-4c72-ae3a-d7817b34d853").unwrap();
    for (endpoint, expected) in [
        (
            EngineEndpoint::FolderStatus(id),
            "/rest/db/status?folder=52f06d0f-8ff6-4c72-ae3a-d7817b34d853",
        ),
        (
            EngineEndpoint::FolderErrors(id),
            "/rest/folder/errors?folder=52f06d0f-8ff6-4c72-ae3a-d7817b34d853&page=1&perpage=128",
        ),
        (
            EngineEndpoint::FolderVersions(id),
            "/rest/folder/versions?folder=52f06d0f-8ff6-4c72-ae3a-d7817b34d853",
        ),
    ] {
        let path = endpoint.path();
        assert_eq!(path, expected);
        assert!(!path.contains('%'));
        assert!(!path.contains('\n'));
        assert_eq!(path.matches('?').count(), 1);
    }
}

#[tokio::test]
async fn rejects_intermediate_symlink_at_creation_and_after_directory_replacement() {
    let server = Server::new();
    let root = std::fs::canonicalize(server._directory.path()).unwrap();
    let actual = root.join("actual");
    let requested = root.join("requested");
    std::fs::create_dir(&actual).unwrap();
    std::fs::set_permissions(&actual, std::fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&actual, &requested).unwrap();
    assert_eq!(
        EngineApiClient::new(requested.join("api.sock"), Zeroizing::new(KEY.to_owned()))
            .unwrap_err(),
        EngineApiError::UnsafeEndpoint
    );
    std::fs::remove_file(&requested).unwrap();
    std::fs::create_dir(&requested).unwrap();
    std::fs::set_permissions(&requested, std::fs::Permissions::from_mode(0o700)).unwrap();
    let client =
        EngineApiClient::new(requested.join("api.sock"), Zeroizing::new(KEY.to_owned())).unwrap();
    std::fs::remove_dir(&requested).unwrap();
    symlink(&actual, &requested).unwrap();
    assert_eq!(
        client
            .json::<serde_json::Value>(EngineEndpoint::SystemStatus)
            .await
            .unwrap_err(),
        EngineApiError::UnsafeEndpoint
    );
}

#[tokio::test]
async fn larger_configuration_cap_does_not_expand_status_responses() {
    let payload = format!("\"{}\"", "a".repeat(MAX_RESPONSE_BYTES + 1));
    for (endpoint, succeeds) in [
        (EngineEndpoint::Configuration, true),
        (EngineEndpoint::SystemStatus, false),
    ] {
        let server = Server::new();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let (result, ()) = tokio::join!(
            server.client.json::<String>(endpoint),
            server.respond(response.as_bytes())
        );
        if succeeds {
            assert_eq!(result.unwrap().len(), MAX_RESPONSE_BYTES + 1);
        } else {
            assert_eq!(result.unwrap_err(), EngineApiError::BodyTooLarge);
        }
    }
    let server = Server::new();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_CONFIGURATION_BYTES + 1
    );
    let (result, ()) = tokio::join!(
        server.client.json::<String>(EngineEndpoint::Configuration),
        server.respond(response.as_bytes())
    );
    assert_eq!(result.unwrap_err(), EngineApiError::BodyTooLarge);
}

#[test]
fn loopback_tls_constructor_rejects_unsafe_addresses_and_der() {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    for address in ["0.0.0.0:8384", "127.0.0.1:0", "[::1%4]:8384"] {
        assert!(matches!(
            EngineApiClient::new_loopback_tls(
                address.parse().unwrap(),
                Zeroizing::new(KEY.to_owned()),
                cert.cert.der().to_vec(),
            ),
            Err(EngineApiError::InvalidConfiguration)
        ));
    }
    let key = Zeroizing::new(KEY.to_owned());
    assert!(matches!(
        EngineApiClient::new_loopback_tls("192.0.2.1:8384".parse().unwrap(), key.clone(), vec![1]),
        Err(EngineApiError::InvalidConfiguration)
    ));
    assert!(matches!(
        EngineApiClient::new_loopback_tls("127.0.0.1:0".parse().unwrap(), key.clone(), vec![1]),
        Err(EngineApiError::InvalidConfiguration)
    ));
    assert!(matches!(
        EngineApiClient::new_loopback_tls("[::1]:8384".parse().unwrap(), key, Vec::new()),
        Err(EngineApiError::InvalidConfiguration)
    ));
}

#[derive(Debug, Default)]
struct TlsObservation {
    handshake_completed: bool,
    application_bytes: usize,
    api_key_matched: bool,
}

async fn tls_server(
    certificate_der: Vec<u8>,
    private_key_der: Vec<u8>,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<TlsObservation>,
) {
    use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(certificate_der)],
            PrivatePkcs8KeyDer::from(private_key_der).into(),
        )
        .unwrap();
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(mut stream) = TlsAcceptor::from(Arc::new(config)).accept(stream).await else {
                return TlsObservation::default();
            };
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            while request.len() < 16 * 1024 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => request.extend_from_slice(&chunk[..count]),
                }
            }
            let api_key_matched = String::from_utf8_lossy(&request).lines().any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("x-api-key") && value.trim() == KEY
                })
            });
            if !request.is_empty() {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{}").await;
            }
            TlsObservation { handshake_completed: true, application_bytes: request.len(), api_key_matched }
        }).await.expect("bounded TLS test server")
    });
    (address, task)
}

#[tokio::test]
async fn loopback_tls_exact_leaf_authenticates_before_http() {
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let expected = cert.cert.der().to_vec();
    let (address, server) =
        tls_server(cert.cert.der().to_vec(), cert.signing_key.serialize_der()).await;
    let client =
        EngineApiClient::new_loopback_tls(address, Zeroizing::new(KEY.into()), expected).unwrap();
    let _: serde_json::Value = client.json(EngineEndpoint::SystemStatus).await.unwrap();
    let observed = server.await.unwrap();
    assert!(observed.handshake_completed);
    assert!(observed.application_bytes > 0);
    assert!(observed.api_key_matched);
}

#[tokio::test]
async fn loopback_tls_wrong_certificate_receives_no_http() {
    let expected =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let wrong =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let (address, server) =
        tls_server(wrong.cert.der().to_vec(), wrong.signing_key.serialize_der()).await;
    let client = EngineApiClient::new_loopback_tls(
        address,
        Zeroizing::new(KEY.into()),
        expected.cert.der().to_vec(),
    )
    .unwrap();
    assert_eq!(
        client.command(EngineEndpoint::Shutdown).await,
        Err(EngineApiError::Unavailable)
    );
    let observed = server.await.unwrap();
    assert!(!observed.handshake_completed);
    assert_eq!(observed.application_bytes, 0);
}

#[tokio::test]
async fn loopback_tls_trusted_ca_different_leaf_is_pinned_before_http() {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_key = KeyPair::generate().unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf_params = CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();
    let (address, server) = tls_server(leaf.der().to_vec(), leaf_key.serialize_der()).await;
    let client =
        EngineApiClient::new_loopback_tls(address, Zeroizing::new(KEY.into()), ca.der().to_vec())
            .unwrap();
    assert_eq!(
        client.command(EngineEndpoint::Shutdown).await,
        Err(EngineApiError::Unavailable)
    );
    let observed = server.await.unwrap();
    // Prove rejection happened after ordinary CA/hostname verification.
    assert!(observed.handshake_completed);
    assert_eq!(observed.application_bytes, 0);
}

#[tokio::test]
async fn loopback_tls_handshake_timeout_closes_accepted_socket() {
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut byte = [0; 1024];
        loop {
            let count = tokio::time::timeout(Duration::from_secs(1), socket.read(&mut byte))
                .await
                .unwrap()
                .unwrap();
            if count == 0 {
                return 0;
            }
        }
    });
    let mut client = EngineApiClient::new_loopback_tls(
        address,
        Zeroizing::new(KEY.into()),
        cert.cert.der().to_vec(),
    )
    .unwrap();
    client.timeout = Duration::from_millis(40);
    assert_eq!(
        client.command(EngineEndpoint::Shutdown).await,
        Err(EngineApiError::Timeout)
    );
    assert_eq!(peer.await.unwrap(), 0);
}

#[tokio::test]
async fn loopback_tls_cancellation_closes_handshake_socket() {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (hello_sent, hello_received) = tokio::sync::oneshot::channel();
    let client = EngineApiClient::new_loopback_tls(
        listener.local_addr().unwrap(),
        Zeroizing::new(KEY.into()),
        cert.cert.der().to_vec(),
    )
    .unwrap();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert!(count > 0);
        hello_sent.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if socket.read(&mut chunk).await.unwrap() == 0 {
                    break;
                }
            }
        })
        .await
        .expect("cancelled handshake must close the socket");
    });
    let request = tokio::spawn(async move { client.command(EngineEndpoint::Shutdown).await });
    tokio::time::timeout(Duration::from_secs(1), hello_received)
        .await
        .unwrap()
        .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    peer.await.unwrap();
}
