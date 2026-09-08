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
