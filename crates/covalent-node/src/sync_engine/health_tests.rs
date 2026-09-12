use super::*;
use std::os::unix::fs::PermissionsExt as _;

use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use zeroize::Zeroizing;

const KEY: &str = "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";

struct Server {
    _directory: TempDir,
    listener: UnixListener,
    client: EngineApiClient,
}

impl Server {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("cvs-health-")
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

    async fn accept(&self) -> (UnixStream, String) {
        let (mut socket, _) = self.listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0_u8; 1024];
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0, "request ended before headers");
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        (socket, String::from_utf8(bytes).unwrap())
    }
}

async fn json(mut socket: UnixStream, value: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{value}",
        value.len()
    );
    socket.write_all(response.as_bytes()).await.unwrap();
    socket.shutdown().await.unwrap();
}

fn status(
    state: &str,
    errors: i64,
    need_files: i64,
    need_bytes: i64,
    error: &str,
    watch_error: &str,
) -> String {
    serde_json::json!({
        "state": state,
        "stateChanged": "2026-09-08T12:34:56.123456789-04:00",
        "errors": errors,
        "needFiles": need_files,
        "needBytes": need_bytes,
        "error": error,
        "watchError": watch_error,
    })
    .to_string()
}

fn errors(folder: Uuid, rows: usize) -> String {
    let rows: Vec<_> = (0..rows)
        .map(|_| serde_json::json!({"path": "/private/folder/nope", "error": "private failure"}))
        .collect();
    serde_json::json!({"folder": folder.to_string(), "errors": rows, "page": 1, "perpage": 128})
        .to_string()
}

#[tokio::test]
async fn observes_folder_progress_and_redacts_scan_and_permission_text() {
    let server = Server::new();
    let folder = Uuid::new_v4();
    let secret = "/private/folder/nope permission denied";
    let server_task = async {
        let (socket, request) = server.accept().await;
        assert!(request.starts_with(&format!("GET /rest/db/status?folder={folder} HTTP/1.1\r\n")));
        json(socket, &status("syncing", 1, 4, 44, secret, "")).await;
        let (socket, request) = server.accept().await;
        assert!(request.starts_with(&format!(
            "GET /rest/folder/errors?folder={folder}&page=1&perpage=128 HTTP/1.1\r\n"
        )));
        json(socket, &errors(folder, 1)).await;
    };
    let folders = [folder];
    let (result, ()) = tokio::join!(collect_folder_health(&server.client, &folders), server_task);
    let result = result.unwrap();
    assert_eq!(result.len(), 1);
    let health = &result[0];
    assert_eq!(health.folder, folder);
    assert_eq!(health.lifecycle, FolderLifecycle::Syncing);
    assert_eq!(health.remaining_files, 4);
    assert_eq!(health.remaining_bytes, 44);
    assert_eq!(health.scan_pull_error_count, 1);
    assert_eq!(health.reported_error_rows, 1);
    assert!(health.status_error);
    assert!(health.has_reported_errors());
    assert!(!format!("{health:?}").contains(secret));
    assert!(!format!("{health:?}").contains("/private/folder"));
}

#[tokio::test]
async fn rejects_missing_or_invalid_critical_status_fields() {
    for body in [
        serde_json::json!({
            "state": "idle", "stateChanged": "2026-09-08T12:34:56Z", "needFiles": 0, "needBytes": 0
        }).to_string(),
        status("unknown-state", 0, 0, 0, "", ""),
        status("idle", 0, -1, 0, "", ""),
        serde_json::json!({
            "state": "idle", "stateChanged": "not-a-time", "errors": 0, "needFiles": 0, "needBytes": 0
        }).to_string(),
    ] {
        let server = Server::new();
        let folder = Uuid::new_v4();
        let responder = async {
            let (socket, _) = server.accept().await;
            json(socket, &body).await;
        };
        let folders = [folder];
        let (result, ()) = tokio::join!(collect_folder_health(&server.client, &folders), responder);
        assert_eq!(result.unwrap_err(), FolderHealthError::InvalidResponse);
    }
}

#[tokio::test]
async fn accepts_the_pinned_null_empty_errors_page_as_observed_zero() {
    let server = Server::new();
    let folder = Uuid::new_v4();
    let responder = async {
        let (socket, _) = server.accept().await;
        json(socket, &status("idle", 0, 0, 0, "", "")).await;
        let (socket, _) = server.accept().await;
        let body = serde_json::json!({
            "folder": folder.to_string(),
            "errors": null,
            "page": 1,
            "perpage": 128,
        })
        .to_string();
        json(socket, &body).await;
    };
    let folders = [folder];
    let (result, ()) = tokio::join!(collect_folder_health(&server.client, &folders), responder);
    let health = result.unwrap().pop().unwrap();
    assert_eq!(health.reported_error_rows, 0);
    assert!(!health.has_reported_errors());
}

#[tokio::test]
async fn refuses_oversized_error_page_and_mismatched_error_envelope() {
    for (rows, wrong_folder) in [(129, false), (0, true)] {
        let server = Server::new();
        let folder = Uuid::new_v4();
        let responder = async {
            let (socket, _) = server.accept().await;
            json(socket, &status("idle", 0, 0, 0, "", "")).await;
            let (socket, _) = server.accept().await;
            let envelope = if wrong_folder {
                errors(Uuid::new_v4(), rows)
            } else {
                errors(folder, rows)
            };
            json(socket, &envelope).await;
        };
        let folders = [folder];
        let (result, ()) = tokio::join!(collect_folder_health(&server.client, &folders), responder);
        assert_eq!(result.unwrap_err(), FolderHealthError::InvalidResponse);
    }
}

#[tokio::test]
async fn rejects_duplicate_or_over_limit_folder_requests_without_connecting() {
    let server = Server::new();
    let folder = Uuid::new_v4();
    assert_eq!(
        collect_folder_health(&server.client, &[folder, folder])
            .await
            .unwrap_err(),
        FolderHealthError::InvalidInput,
    );
    assert_eq!(
        collect_folder_health(&server.client, &vec![Uuid::new_v4(); 129])
            .await
            .unwrap_err(),
        FolderHealthError::InvalidInput,
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), server.listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn starts_no_more_than_four_exchanges_before_a_batch_can_complete() {
    let server = Server::new();
    let folders: Vec<_> = (0..5).map(|_| Uuid::new_v4()).collect();
    let responder = async {
        let mut initial = Vec::new();
        for _ in 0..4 {
            let (socket, request) = server.accept().await;
            assert!(request.starts_with("GET /rest/db/status?folder="));
            initial.push((socket, request));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(40), server.listener.accept())
                .await
                .is_err()
        );
        for (socket, _) in initial {
            json(socket, &status("idle", 0, 0, 0, "", "")).await;
        }
        for _ in 0..4 {
            let (socket, request) = server.accept().await;
            let target = request.split_whitespace().nth(1).unwrap();
            let id = target
                .split("folder=")
                .nth(1)
                .unwrap()
                .split('&')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(target.starts_with("/rest/folder/errors?"));
            json(socket, &errors(id, 0)).await;
        }
        let (socket, request) = server.accept().await;
        assert!(request.starts_with(&format!(
            "GET /rest/db/status?folder={} HTTP/1.1\r\n",
            folders[4]
        )));
        json(socket, &status("idle", 0, 0, 0, "", "")).await;
        let (socket, request) = server.accept().await;
        assert!(request.starts_with(&format!(
            "GET /rest/folder/errors?folder={}&page=1&perpage=128 HTTP/1.1\r\n",
            folders[4]
        )));
        json(socket, &errors(folders[4], 0)).await;
    };
    let (result, ()) = tokio::join!(collect_folder_health(&server.client, &folders), responder);
    assert_eq!(result.unwrap().len(), folders.len());
}
