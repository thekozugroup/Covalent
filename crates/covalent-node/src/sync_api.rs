//! Authenticated, redacted folder-sync status for native hosts and the console.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};

use crate::{ApiError, AppState, ContractJson, authorize};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OfferRequest {
    peer_id: covalent_protocol::DeviceId,
    folder_id: uuid::Uuid,
    label: String,
    selected_root: std::path::PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AcceptRequest {
    offer_id: uuid::Uuid,
    selected_root: std::path::PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ShareRequest {
    offer_id: uuid::Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PauseRequest {
    offer_id: uuid::Uuid,
    paused: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MutationResponse {
    schema_version: u16,
    offer_id: Option<uuid::Uuid>,
    lifecycle: &'static str,
    issue: Option<&'static str>,
}

#[cfg(unix)]
fn mutation_response(
    offer_id: Option<uuid::Uuid>,
    lifecycle: crate::sync_engine::FolderSyncLifecycle,
) -> axum::Json<MutationResponse> {
    let (lifecycle, issue) = lifecycle_fields(lifecycle);
    axum::Json(MutationResponse {
        schema_version: 1,
        offer_id,
        lifecycle,
        issue,
    })
}

#[cfg(unix)]
fn ready_service(
    state: &AppState,
) -> Result<&std::sync::Arc<crate::sync_engine::FolderSyncService>, ApiError> {
    if let crate::sync_engine::FolderSyncRuntimeState::Ready(service) = &state.folder_sync {
        Ok(service)
    } else {
        Err(unavailable_error())
    }
}

fn unavailable_error() -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        code: "folder_sync_unavailable",
        message: "Folder sync is unavailable on this device.",
        retryable: false,
        upload_offset: None,
    }
}

pub(crate) async fn offer(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<OfferRequest>,
) -> Result<axum::Json<MutationResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        let committed = ready_service(&state)?
            .offer(
                request.peer_id,
                request.folder_id,
                &request.label,
                &request.selected_root,
                crate::now_unix_ms(),
            )
            .await
            .map_err(service_error)?;
        Ok(mutation_response(
            Some(committed.value().offer_id),
            committed.lifecycle(),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(unavailable_error())
    }
}

pub(crate) async fn accept(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<AcceptRequest>,
) -> Result<axum::Json<MutationResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        let committed = ready_service(&state)?
            .accept(
                request.offer_id,
                &request.selected_root,
                crate::now_unix_ms(),
            )
            .await
            .map_err(service_error)?;
        Ok(mutation_response(
            Some(request.offer_id),
            committed.lifecycle(),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(unavailable_error())
    }
}

pub(crate) async fn pause(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<PauseRequest>,
) -> Result<axum::Json<MutationResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        let committed = ready_service(&state)?
            .pause(request.offer_id, request.paused)
            .await
            .map_err(service_error)?;
        Ok(mutation_response(
            Some(request.offer_id),
            committed.lifecycle(),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(unavailable_error())
    }
}

pub(crate) async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<ShareRequest>,
) -> Result<axum::Json<MutationResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        let committed = ready_service(&state)?
            .remove(request.offer_id)
            .await
            .map_err(service_error)?;
        Ok(mutation_response(
            Some(request.offer_id),
            committed.lifecycle(),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(unavailable_error())
    }
}

pub(crate) async fn retry(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<MutationResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        let lifecycle = ready_service(&state)?
            .start()
            .await
            .map_err(service_error)?;
        Ok(mutation_response(None, lifecycle))
    }
    #[cfg(not(unix))]
    {
        Err(unavailable_error())
    }
}

#[cfg(unix)]
fn lifecycle_fields(
    lifecycle: crate::sync_engine::FolderSyncLifecycle,
) -> (&'static str, Option<&'static str>) {
    use crate::sync_engine::{FolderSyncIssue, FolderSyncLifecycle};
    match lifecycle {
        FolderSyncLifecycle::Stopped => ("stopped", None),
        FolderSyncLifecycle::InitialScanning => ("initialScanning", None),
        FolderSyncLifecycle::Running => ("running", None),
        FolderSyncLifecycle::StillStopping => ("stillStopping", None),
        FolderSyncLifecycle::NeedsAttention(issue) => (
            "needsAttention",
            Some(match issue {
                FolderSyncIssue::Journal => "journal",
                FolderSyncIssue::WorkerLaunch => "workerLaunch",
                FolderSyncIssue::InitialScan => "initialScan",
                FolderSyncIssue::WorkerHealth => "workerHealth",
                FolderSyncIssue::WorkerStop => "workerStop",
                FolderSyncIssue::PeerRevocation => "peerRevocation",
            }),
        ),
    }
}

#[cfg(all(test, unix))]
mod lifecycle_tests {
    use super::lifecycle_fields;
    use crate::sync_engine::{FolderSyncIssue, FolderSyncLifecycle};

    #[test]
    fn initial_scan_states_have_distinct_stable_wire_values() {
        assert_eq!(
            lifecycle_fields(FolderSyncLifecycle::InitialScanning),
            ("initialScanning", None)
        );
        assert_eq!(
            lifecycle_fields(FolderSyncLifecycle::NeedsAttention(
                FolderSyncIssue::InitialScan
            )),
            ("needsAttention", Some("initialScan"))
        );
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncStatusResponse {
    schema_version: u16,
    availability: &'static str,
    lifecycle: &'static str,
    issue: Option<&'static str>,
    health_freshness: &'static str,
    peers: Vec<PeerResponse>,
    shares: Vec<ShareResponse>,
    folders: Vec<FolderResponse>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PeerResponse {
    peer_id: covalent_protocol::DeviceId,
    display_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShareResponse {
    offer_id: uuid::Uuid,
    folder_id: uuid::Uuid,
    label: String,
    peer_id: covalent_protocol::DeviceId,
    incoming: bool,
    phase: &'static str,
    expires_at_unix_ms: Option<u64>,
    expired: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderResponse {
    folder_id: uuid::Uuid,
    state: &'static str,
    state_changed: String,
    remaining_files: u64,
    remaining_bytes: u64,
    scan_pull_error_count: u64,
    reported_error_rows: u16,
    status_error: bool,
    watch_error: bool,
}

impl SyncStatusResponse {
    fn unavailable(availability: &'static str, issue: Option<&'static str>) -> Self {
        Self {
            schema_version: 1,
            availability,
            lifecycle: "stopped",
            issue,
            health_freshness: "neverObserved",
            peers: Vec::new(),
            shares: Vec::new(),
            folders: Vec::new(),
        }
    }
}

pub(crate) async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<SyncStatusResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        use crate::sync_engine::{
            FolderHealthFreshness, FolderLifecycle, FolderSyncRuntimeState, SharingPhase,
        };
        let service = match &state.folder_sync {
            FolderSyncRuntimeState::NotPackaged => {
                return Ok(axum::Json(SyncStatusResponse::unavailable(
                    "notPackaged",
                    None,
                )));
            }
            FolderSyncRuntimeState::NeedsAttention => {
                return Ok(axum::Json(SyncStatusResponse::unavailable(
                    "needsAttention",
                    Some("installation"),
                )));
            }
            FolderSyncRuntimeState::FolderAccessUnavailable => {
                return Ok(axum::Json(SyncStatusResponse::unavailable(
                    "needsAttention",
                    Some("folderAccess"),
                )));
            }
            FolderSyncRuntimeState::Ready(service) => service,
        };
        let snapshot = service.status().await.map_err(service_error)?;
        let config = state.engine.config().map_err(ApiError::from_core)?;
        let peers = config
            .trusted_peers
            .iter()
            .filter(|(id, grant)| {
                !grant.revoked
                    && grant.confirmed_at_unix_ms != 0
                    && grant.peer_device_id == **id
                    && config
                        .trusted_peer_transports
                        .get(id)
                        .is_some_and(|pin| pin.peer_id == **id)
            })
            .map(|(id, grant)| PeerResponse {
                peer_id: *id,
                display_name: grant.display_name.clone(),
            })
            .collect();
        let (lifecycle, issue) = lifecycle_fields(snapshot.lifecycle());
        let now = crate::now_unix_ms();
        let shares = snapshot
            .shares()
            .iter()
            .map(|share| ShareResponse {
                offer_id: share.offer_id,
                folder_id: share.folder_id,
                label: share.label.clone(),
                peer_id: share.peer_id,
                incoming: share.incoming,
                expires_at_unix_ms: share.expires_at_unix_ms,
                expired: share
                    .expires_at_unix_ms
                    .is_some_and(|expires| now >= expires),
                phase: match share.phase {
                    SharingPhase::Offered => "offered",
                    SharingPhase::AwaitingCommit => "awaitingCommit",
                    SharingPhase::Ready => "ready",
                    SharingPhase::Paused => "paused",
                    SharingPhase::Removed => "removed",
                },
            })
            .collect();
        let folders = snapshot
            .folder_health()
            .iter()
            .map(|folder| {
                Ok(FolderResponse {
                    folder_id: folder.folder,
                    state: match folder.lifecycle {
                        FolderLifecycle::Starting => "starting",
                        FolderLifecycle::Idle => "idle",
                        FolderLifecycle::Scanning => "scanning",
                        FolderLifecycle::ScanWaiting => "scan-waiting",
                        FolderLifecycle::SyncWaiting => "sync-waiting",
                        FolderLifecycle::SyncPreparing => "sync-preparing",
                        FolderLifecycle::Syncing => "syncing",
                        FolderLifecycle::Cleaning => "cleaning",
                        FolderLifecycle::CleanWaiting => "clean-waiting",
                        FolderLifecycle::Error => "error",
                    },
                    state_changed: folder
                        .state_changed
                        .format(&time::format_description::well_known::Rfc3339)
                        .map_err(|_| {
                            service_error(crate::sync_engine::FolderSyncServiceError::Journal)
                        })?,
                    remaining_files: folder.remaining_files,
                    remaining_bytes: folder.remaining_bytes,
                    scan_pull_error_count: folder.scan_pull_error_count,
                    reported_error_rows: folder.reported_error_rows,
                    status_error: folder.status_error,
                    watch_error: folder.watch_error,
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        Ok(axum::Json(SyncStatusResponse {
            schema_version: 1,
            availability: "available",
            lifecycle,
            issue,
            peers,
            health_freshness: match snapshot.health_freshness() {
                FolderHealthFreshness::NeverObserved => "neverObserved",
                FolderHealthFreshness::Fresh => "fresh",
                FolderHealthFreshness::Stale => "stale",
            },
            shares,
            folders,
        }))
    }
    #[cfg(not(unix))]
    Ok(axum::Json(SyncStatusResponse::unavailable(
        "notPackaged",
        None,
    )))
}

#[cfg(unix)]
pub(crate) fn service_error(error: crate::sync_engine::FolderSyncServiceError) -> ApiError {
    use crate::sync_engine::FolderSyncServiceError;
    let retryable = matches!(
        error,
        FolderSyncServiceError::Busy | FolderSyncServiceError::WorkerStillStopping
    );
    ApiError {
        status: if retryable {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::CONFLICT
        },
        code: if retryable {
            "folder_sync_busy"
        } else {
            "folder_sync_needs_attention"
        },
        message: if retryable {
            "Folder sync is busy. Try again shortly."
        } else {
            "Folder sync needs attention before it can continue."
        },
        retryable,
        upload_offset: None,
    }
}
