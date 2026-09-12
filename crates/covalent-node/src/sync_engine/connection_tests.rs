use super::config::EngineDeviceId;
use super::*;
use std::os::unix::fs::PermissionsExt as _;

use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixListener;
use zeroize::Zeroizing;

const KEY: &str = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
const FIRST: &str = "EA2I6UL-UUWKJDX-NGOC6QS-U2IZR4Z-LXY5CB7-3KBVQRG-4CNFBV7-RACTXQF";
const SECOND: &str = "FZ23CS4-PDV743V-32LS44M-TCIUBAQ-RQ3OC4X-XN6EG66-BGUCZH4-OISTMAE";

struct Server {
    _directory: TempDir,
    listener: UnixListener,
    client: EngineApiClient,
}

impl Server {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("cvs-connections-")
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

    async fn serve(&self, body: &str) {
        let (mut socket, _) = self.listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0_u8; 1024];
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            String::from_utf8(request)
                .unwrap()
                .starts_with("GET /rest/system/connections HTTP/1.1\r\n")
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    }
}

fn id(value: &str) -> EngineDeviceId {
    EngineDeviceId::parse(value).unwrap()
}

#[tokio::test]
async fn retains_only_exact_configured_connection_states() {
    let server = Server::new();
    let body = serde_json::json!({
        "connections": {
            FIRST: {"connected": true, "paused": false, "address": "tcp://private:22000", "clientVersion": "secret"},
            SECOND: {"connected": true, "paused": true, "at": "private"}
        },
        "total": {"at": "private", "inBytesTotal": 42}
    }).to_string();
    let peers = [id(FIRST), id(SECOND)];
    let (result, ()) = tokio::join!(
        collect_peer_connections(&server.client, &peers),
        server.serve(&body)
    );
    let result = result.unwrap();
    assert_eq!(result[0].state(), EnginePeerConnectionState::Connected);
    assert_eq!(result[1].state(), EnginePeerConnectionState::Paused);
    let debug = format!("{result:?}");
    assert!(!debug.contains(FIRST));
    assert!(!debug.contains("private"));
}

#[tokio::test]
async fn rejects_missing_extra_and_malformed_connection_entries() {
    let cases = [
        serde_json::json!({"connections": {}, "total": {}}),
        serde_json::json!({"connections": {FIRST: {"connected": false, "paused": false}, SECOND: {"connected": false, "paused": false}}, "total": {}}),
        serde_json::json!({"connections": {FIRST: {"connected": "yes", "paused": false}}, "total": {}}),
        serde_json::json!({"connections": {FIRST: {"connected": false}}, "total": {}}),
    ];
    for body in cases {
        let server = Server::new();
        let encoded = body.to_string();
        let peers = [id(FIRST)];
        let (result, ()) = tokio::join!(
            collect_peer_connections(&server.client, &peers),
            server.serve(&encoded)
        );
        assert_eq!(result.unwrap_err(), PeerConnectionError::InvalidResponse);
    }

    let server = Server::new();
    let duplicate = format!(
        "{{\"connections\":{{\"{FIRST}\":{{\"connected\":false,\"paused\":false}},\"{FIRST}\":{{\"connected\":true,\"paused\":false}}}},\"total\":{{}}}}"
    );
    let peers = [id(FIRST)];
    let (result, ()) = tokio::join!(
        collect_peer_connections(&server.client, &peers),
        server.serve(&duplicate)
    );
    assert_eq!(result.unwrap_err(), PeerConnectionError::InvalidResponse);
}

#[tokio::test]
async fn input_is_nonempty_unique_and_bounded_before_network() {
    let server = Server::new();
    assert_eq!(
        collect_peer_connections(&server.client, &[])
            .await
            .unwrap_err(),
        PeerConnectionError::InvalidInput
    );
    let first = id(FIRST);
    assert_eq!(
        collect_peer_connections(&server.client, &[first.clone(), first])
            .await
            .unwrap_err(),
        PeerConnectionError::InvalidInput
    );
    assert_eq!(
        collect_peer_connections(&server.client, &vec![id(FIRST); 129])
            .await
            .unwrap_err(),
        PeerConnectionError::InvalidInput
    );
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(30),
            server.listener.accept()
        )
        .await
        .is_err()
    );
}
