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
pub(crate) struct RepairRequest {
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RefreshPeerAddressRequest {
    peer_id: covalent_protocol::DeviceId,
    expected_address: String,
    candidate_address: String,
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

/// Replace an expired outgoing invitation after an explicit local request.
/// The returned offer ID names the newly signed invitation; callers must
/// durably replace their pending draft reference with that value.
pub(crate) async fn renew(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<ShareRequest>,
) -> Result<axum::Json<MutationResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        let committed = ready_service(&state)?
            .renew_offer(request.offer_id, crate::now_unix_ms())
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
        use crate::sync_engine::FolderSyncRuntimeState;
        match &state.folder_sync {
            FolderSyncRuntimeState::Ready(service) => {
                let committed = service
                    .remove(request.offer_id)
                    .await
                    .map_err(service_error)?;
                Ok(mutation_response(
                    Some(request.offer_id),
                    committed.lifecycle(),
                ))
            }
            FolderSyncRuntimeState::FolderAccessUnavailable(Some(recovery)) => {
                recovery
                    .remove(request.offer_id)
                    .await
                    .map_err(sharing_error)?;
                Ok(folder_access_mutation_response(request.offer_id))
            }
            _ => Err(unavailable_error()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(unavailable_error())
    }
}

pub(crate) async fn repair(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RepairRequest>,
) -> Result<axum::Json<MutationResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        use crate::sync_engine::FolderSyncRuntimeState;
        match &state.folder_sync {
            FolderSyncRuntimeState::Ready(service) => {
                let committed = service
                    .repair_root(request.offer_id, &request.selected_root)
                    .await
                    .map_err(service_error)?;
                Ok(mutation_response(
                    Some(request.offer_id),
                    committed.lifecycle(),
                ))
            }
            FolderSyncRuntimeState::FolderAccessUnavailable(Some(recovery)) => {
                recovery
                    .repair_root(request.offer_id, &request.selected_root)
                    .await
                    .map_err(sharing_error)?;
                Ok(folder_access_mutation_response(request.offer_id))
            }
            _ => Err(unavailable_error()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(unavailable_error())
    }
}

#[cfg(unix)]
fn folder_access_mutation_response(offer_id: uuid::Uuid) -> axum::Json<MutationResponse> {
    axum::Json(MutationResponse {
        schema_version: 1,
        offer_id: Some(offer_id),
        lifecycle: "stopped",
        issue: Some("folderAccess"),
    })
}

pub(crate) async fn retry(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<MutationResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        let service = ready_service(&state)?;
        state
            .finish_peer_address_provider_barrier(service)
            .await
            .map_err(ApiError::from_core)?;
        let lifecycle = service.start().await.map_err(service_error)?;
        Ok(mutation_response(None, lifecycle))
    }
    #[cfg(not(unix))]
    {
        Err(unavailable_error())
    }
}

/// Authenticate the same pinned peer certificate and signing identity at a
/// candidate numeric endpoint before advancing mutable route state. Historical
/// pairing and folder signatures remain unchanged.
pub(crate) async fn refresh_peer_address(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RefreshPeerAddressRequest>,
) -> Result<axum::Json<MutationResponse>, ApiError> {
    authorize(&state, &headers)?;
    #[cfg(unix)]
    {
        let candidate = request
            .candidate_address
            .parse::<std::net::SocketAddr>()
            .map_err(|_| invalid_address_error())?;
        if candidate.to_string() != request.candidate_address {
            return Err(invalid_address_error());
        }
        let config = state.engine.config().map_err(ApiError::from_core)?;
        let current = config
            .trusted_peer_transports
            .get(&request.peer_id)
            .cloned()
            .ok_or_else(untrusted_peer_error)?;
        let current_grant = config
            .trusted_peers
            .get(&request.peer_id)
            .cloned()
            .ok_or_else(untrusted_peer_error)?;
        let service = ready_service(&state)?;
        if current.address == request.candidate_address
            && request.expected_address == current.address
            && service
                .pending_peer_address_refresh()
                .await
                .map_err(service_error)?
                == Some((request.peer_id, true))
        {
            state
                .finish_peer_address_provider_barrier(service)
                .await
                .map_err(ApiError::from_core)?;
            let lifecycle = service.start().await.map_err(service_error)?;
            return Ok(mutation_response(None, lifecycle));
        }
        if current.address != request.expected_address {
            if current.address != request.candidate_address
                || !service
                    .recognizes_peer_address_refresh(
                        request.peer_id,
                        &request.expected_address,
                        &request.candidate_address,
                    )
                    .await
                    .map_err(service_error)?
            {
                return Err(untrusted_peer_error());
            }
            state
                .refresh_provider_from_current_trust(request.peer_id)
                .map_err(ApiError::from_core)?;
            if service
                .pending_peer_address_refresh()
                .await
                .map_err(service_error)?
                .is_some()
            {
                service
                    .finish_peer_address_refresh(request.peer_id)
                    .await
                    .map_err(service_error)?;
            }
            let lifecycle = service.start().await.map_err(service_error)?;
            return Ok(mutation_response(None, lifecycle));
        }
        let candidate_engine = crate::sync_control::send_peer_address_probe(
            state.engine(),
            current.clone(),
            candidate,
        )
        .await
        .map_err(|_| address_probe_error())?;
        let mut replacement = current.clone();
        replacement.address = request.candidate_address;
        let committed = service
            .refresh_peer_address(
                request.peer_id,
                &current_grant,
                &current,
                &replacement,
                &candidate_engine,
            )
            .await
            .map_err(service_error)?;
        state
            .refresh_provider_from_current_trust(request.peer_id)
            .map_err(ApiError::from_core)?;
        service
            .finish_peer_address_refresh(request.peer_id)
            .await
            .map_err(service_error)?;
        let lifecycle = match service.start().await {
            Ok(lifecycle) => lifecycle,
            Err(_) => service
                .status()
                .await
                .map(|status| status.lifecycle())
                .unwrap_or(committed.lifecycle()),
        };
        Ok(mutation_response(None, lifecycle))
    }
    #[cfg(not(unix))]
    {
        let _ = request;
        Err(unavailable_error())
    }
}

fn invalid_address_error() -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_peer_address",
        message: "The new device address is invalid.",
        retryable: false,
        upload_offset: None,
    }
}

fn untrusted_peer_error() -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        code: "peer_address_changed",
        message: "The trusted device address changed before this update.",
        retryable: false,
        upload_offset: None,
    }
}

fn address_probe_error() -> ApiError {
    ApiError {
        status: StatusCode::BAD_GATEWAY,
        code: "peer_address_unreachable",
        message: "The device could not be authenticated at the new address.",
        retryable: true,
        upload_offset: None,
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

#[cfg(unix)]
fn peer_connection_field(
    phase: crate::sync_engine::SharingPhase,
    observed: crate::sync_engine::PeerConnectionState,
) -> &'static str {
    use crate::sync_engine::{PeerConnectionState, SharingPhase};
    match phase {
        SharingPhase::Paused => "paused",
        SharingPhase::Ready => match observed {
            PeerConnectionState::Unknown => "unknown",
            PeerConnectionState::Connected => "connected",
            PeerConnectionState::Disconnected => "disconnected",
            PeerConnectionState::Paused => "paused",
        },
        SharingPhase::Offered | SharingPhase::AwaitingCommit | SharingPhase::Removed => "unknown",
    }
}

#[cfg(all(test, unix))]
mod lifecycle_tests {
    use std::collections::BTreeSet;

    use covalent_core::{DeviceIdentity, NodeConfig};
    use covalent_protocol::{PeerGrant, TransportBinding};

    use super::{lifecycle_fields, peer_connection_field, peer_responses};
    use crate::sync_engine::{
        FolderSyncIssue, FolderSyncLifecycle, PeerConnectionState, SharingPhase,
    };

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

    #[test]
    fn pending_and_removed_shares_never_inherit_an_active_peer_connection() {
        for phase in [
            SharingPhase::Offered,
            SharingPhase::AwaitingCommit,
            SharingPhase::Removed,
        ] {
            assert_eq!(
                peer_connection_field(phase, PeerConnectionState::Connected),
                "unknown"
            );
        }
        assert_eq!(
            peer_connection_field(SharingPhase::Paused, PeerConnectionState::Connected),
            "paused"
        );
        assert_eq!(
            peer_connection_field(SharingPhase::Ready, PeerConnectionState::Disconnected),
            "disconnected"
        );
    }

    #[test]
    fn peer_status_uses_only_the_current_trusted_pin_address() {
        let current = DeviceIdentity::generate().public_identity();
        let unpinned = DeviceIdentity::generate().public_identity();
        let revoked = DeviceIdentity::generate().public_identity();
        let grant = |identity: &covalent_core::PublicIdentity, name: &str, revoked| PeerGrant {
            peer_device_id: identity.device_id,
            public_key: identity.public_key.clone(),
            display_name: name.to_owned(),
            roles: BTreeSet::new(),
            confirmed_at_unix_ms: 1,
            revoked,
        };
        let binding = |identity: &covalent_core::PublicIdentity, name: &str, address: &str| {
            TransportBinding {
                peer_id: identity.device_id,
                display_name: name.to_owned(),
                address: address.to_owned(),
                certificate_der: "not-returned-by-status".to_owned(),
                certificate_fingerprint: "also-not-returned".to_owned(),
            }
        };
        let mut config = NodeConfig::new("local", false).expect("config");
        config
            .trusted_peers
            .insert(current.device_id, grant(&current, "Current peer", false));
        config
            .trusted_peers
            .insert(unpinned.device_id, grant(&unpinned, "Unpinned peer", false));
        config
            .trusted_peers
            .insert(revoked.device_id, grant(&revoked, "Revoked peer", true));
        config.trusted_peer_transports.insert(
            current.device_id,
            binding(&current, "Current peer", "127.0.0.1:55101"),
        );
        config.trusted_peer_transports.insert(
            revoked.device_id,
            binding(&revoked, "Revoked peer", "127.0.0.1:55103"),
        );

        let before = serde_json::to_value(peer_responses(&config)).expect("status peers");
        assert_eq!(before.as_array().expect("peers").len(), 1);
        assert_eq!(before[0]["peerId"], current.device_id.to_string());
        assert_eq!(before[0]["displayName"], "Current peer");
        assert_eq!(before[0]["address"], "127.0.0.1:55101");
        assert!(before[0].get("certificateDer").is_none());
        assert!(before[0].get("certificateFingerprint").is_none());
        assert!(before[0].get("publicKey").is_none());

        // The address-refresh transaction replaces only this current pin. The
        // status projection must immediately use that durable value rather
        // than a historical pairing receipt, provider cache or discovery row.
        config
            .trusted_peer_transports
            .get_mut(&current.device_id)
            .expect("current pin")
            .address = "127.0.0.1:55112".to_owned();
        let after = serde_json::to_value(peer_responses(&config)).expect("refreshed peers");
        assert_eq!(after[0]["address"], "127.0.0.1:55112");

        config.trusted_peer_transports.remove(&current.device_id);
        assert!(peer_responses(&config).is_empty());
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
    connection_freshness: &'static str,
    peers: Vec<PeerResponse>,
    shares: Vec<ShareResponse>,
    folders: Vec<FolderResponse>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PeerResponse {
    peer_id: covalent_protocol::DeviceId,
    display_name: String,
    address: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShareResponse {
    offer_id: uuid::Uuid,
    superseded_offer_ids: Vec<uuid::Uuid>,
    folder_id: uuid::Uuid,
    label: String,
    peer_id: covalent_protocol::DeviceId,
    incoming: bool,
    phase: &'static str,
    expires_at_unix_ms: Option<u64>,
    expired: bool,
    peer_connection: &'static str,
    remote_removal_pending: bool,
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
            connection_freshness: "neverObserved",
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
            FolderHealthFreshness, FolderLifecycle, FolderSyncRuntimeState, PeerConnectionFreshness,
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
            FolderSyncRuntimeState::FolderAccessUnavailable(recovery) => {
                let Some(recovery) = recovery else {
                    return Ok(axum::Json(SyncStatusResponse::unavailable(
                        "needsAttention",
                        Some("folderAccess"),
                    )));
                };
                let summaries = recovery.summaries().await.map_err(sharing_error)?;
                let config = state.engine.config().map_err(ApiError::from_core)?;
                return Ok(axum::Json(SyncStatusResponse {
                    schema_version: 1,
                    availability: "needsAttention",
                    lifecycle: "stopped",
                    issue: Some("folderAccess"),
                    health_freshness: "neverObserved",
                    connection_freshness: "neverObserved",
                    peers: peer_responses(&config),
                    shares: share_responses(&summaries, |_| {
                        crate::sync_engine::PeerConnectionState::Unknown
                    }),
                    folders: Vec::new(),
                }));
            }
            FolderSyncRuntimeState::Ready(service) => service,
        };
        let snapshot = service.status().await.map_err(service_error)?;
        let config = state.engine.config().map_err(ApiError::from_core)?;
        let peers = peer_responses(&config);
        let (lifecycle, issue) = lifecycle_fields(snapshot.lifecycle());
        let shares = share_responses(snapshot.shares(), |peer| snapshot.peer_connection(peer));
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
            connection_freshness: match snapshot.connection_freshness() {
                PeerConnectionFreshness::NeverObserved => "neverObserved",
                PeerConnectionFreshness::Fresh => "fresh",
                PeerConnectionFreshness::Stale => "stale",
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

#[cfg(unix)]
fn sharing_error(_: crate::sync_engine::SharingError) -> ApiError {
    service_error(crate::sync_engine::FolderSyncServiceError::Journal)
}

#[cfg(unix)]
fn peer_responses(config: &covalent_core::NodeConfig) -> Vec<PeerResponse> {
    config
        .trusted_peers
        .iter()
        .filter_map(|(id, grant)| {
            let pin = config.trusted_peer_transports.get(id)?;
            (!grant.revoked
                && grant.confirmed_at_unix_ms != 0
                && grant.peer_device_id == *id
                && pin.peer_id == *id)
                .then(|| PeerResponse {
                    peer_id: *id,
                    display_name: grant.display_name.clone(),
                    address: pin.address.clone(),
                })
        })
        .collect()
}

#[cfg(unix)]
fn share_responses(
    summaries: &[crate::sync_engine::ShareSummary],
    connection: impl Fn(covalent_protocol::DeviceId) -> crate::sync_engine::PeerConnectionState,
) -> Vec<ShareResponse> {
    use crate::sync_engine::SharingPhase;
    let now = crate::now_unix_ms();
    summaries
        .iter()
        .map(|share| ShareResponse {
            offer_id: share.offer_id,
            superseded_offer_ids: share.superseded_offer_ids.clone(),
            folder_id: share.folder_id,
            label: share.label.clone(),
            peer_id: share.peer_id,
            incoming: share.incoming,
            expires_at_unix_ms: share.expires_at_unix_ms,
            expired: share
                .expires_at_unix_ms
                .is_some_and(|expires| now >= expires),
            peer_connection: peer_connection_field(share.phase, connection(share.peer_id)),
            remote_removal_pending: share.remote_removal_pending,
            phase: match share.phase {
                SharingPhase::Offered => "offered",
                SharingPhase::AwaitingCommit => "awaitingCommit",
                SharingPhase::Ready => "ready",
                SharingPhase::Paused => "paused",
                SharingPhase::Removed => "removed",
            },
        })
        .collect()
}
