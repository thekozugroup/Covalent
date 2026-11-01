//! A local-owner controller bond, separate from ordinary sync pairing.

use std::fs::File;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use covalent_core::{Engine, PublicIdentity};
use covalent_protocol::DeviceId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ApiError, AppState, ContractJson, authorize};

const MAX_BOND_BYTES: u64 = 8192;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ControllerBond {
    schema_version: u16,
    generation: Uuid,
    appliance: PublicIdentity,
    #[serde(deserialize_with = "required_nullable")]
    controller: Option<PublicIdentity>,
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BondError {
    Unavailable,
    Rejected,
    Conflict,
}

pub(crate) struct ControllerBondStore {
    path: PathBuf,
    _lock: File,
    // None is an I/O failure, never an invitation to replace a saved bond.
    bond: Mutex<Option<ControllerBond>>,
    pub(crate) platform: OnceLock<Arc<dyn crate::appliance_platform::AppliancePlatformReader>>,
    pub(crate) imports: OnceLock<Arc<dyn crate::appliance_imports::ApplianceImportHost>>,
    pub(crate) updates: OnceLock<Arc<dyn crate::appliance_updates::ApplianceUpdateHost>>,
    host_ops: Arc<tokio::sync::Semaphore>,
    platform_reads: Arc<tokio::sync::Semaphore>,
}

impl ControllerBondStore {
    pub(crate) fn open(path: PathBuf, appliance: PublicIdentity) -> Option<Self> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};

        let descriptor = open(
            path.with_extension("lock"),
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .ok()?;
        let stat = fstat(&descriptor).ok()?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_nlink != 1
            || stat.st_uid != rustix::process::geteuid().as_raw()
            || stat.st_mode & 0o077 != 0
            || stat.st_size != 0
        {
            return None;
        }
        let lock = File::from(descriptor);
        // One authority store per node; a second instance must not overwrite stale consent.
        fs2::FileExt::try_lock_exclusive(&lock).ok()?;
        let bond = match open(
            &path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Err(rustix::io::Errno::NOENT) => ControllerBond {
                schema_version: 1,
                generation: Uuid::new_v4(),
                appliance: appliance.clone(),
                controller: None,
            },
            Err(_) => return None,
            Ok(descriptor) => {
                let stat = fstat(&descriptor).ok()?;
                if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
                    || stat.st_nlink != 1
                    || stat.st_uid != rustix::process::geteuid().as_raw()
                    || stat.st_mode & 0o077 != 0
                    || stat.st_size < 0
                    || stat.st_size as u64 > MAX_BOND_BYTES
                {
                    return None;
                }
                let mut bytes = Vec::new();
                File::from(descriptor)
                    .take(MAX_BOND_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                if bytes.len() as u64 > MAX_BOND_BYTES {
                    return None;
                }
                let bond: ControllerBond = serde_json::from_slice(&bytes).ok()?;
                if bond.schema_version != 1
                    || bond.generation.is_nil()
                    || bond.appliance != appliance
                {
                    return None;
                }
                if let Some(controller) = &bond.controller
                    && (controller.device_id == DeviceId::from_uuid(Uuid::nil())
                        || controller.device_id == appliance.device_id
                        || controller.verifying_key().is_err())
                {
                    return None;
                }
                bond
            }
        };
        Some(Self {
            path,
            _lock: lock,
            bond: Mutex::new(Some(bond)),
            platform: OnceLock::new(),
            imports: OnceLock::new(),
            updates: OnceLock::new(),
            host_ops: Arc::new(tokio::sync::Semaphore::new(1)),
            platform_reads: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    fn snapshot(&self) -> Result<ControllerBond, BondError> {
        self.bond
            .lock()
            .map_err(|_| BondError::Unavailable)?
            .clone()
            .ok_or(BondError::Unavailable)
    }

    fn save(
        &self,
        current: &mut Option<ControllerBond>,
        next: ControllerBond,
    ) -> Result<ControllerBond, BondError> {
        let bytes = serde_json::to_vec(&next).map_err(|_| BondError::Unavailable)?;
        if crate::persist_private_file(&self.path, &bytes).is_err() {
            *current = None;
            return Err(BondError::Unavailable);
        }
        *current = Some(next.clone());
        Ok(next)
    }

    fn grant(
        &self,
        engine: &Engine,
        generation: Uuid,
        controller: PublicIdentity,
    ) -> Result<ControllerBond, BondError> {
        let identity = crate::sync_control::current_identity(engine, controller.device_id)
            .map_err(|_| BondError::Rejected)?;
        if identity != controller || controller.device_id == engine.device_id() {
            return Err(BondError::Rejected);
        }
        let mut current = self.bond.lock().map_err(|_| BondError::Unavailable)?;
        let bond = current.as_ref().ok_or(BondError::Unavailable)?;
        if bond.appliance != engine.public_identity() {
            return Err(BondError::Conflict);
        }
        if let Some(existing) = &bond.controller {
            return if existing == &controller {
                Ok(bond.clone())
            } else {
                Err(BondError::Conflict)
            };
        }
        if bond.generation != generation {
            return Err(BondError::Conflict);
        }
        let next = ControllerBond {
            generation: Uuid::new_v4(),
            controller: Some(controller),
            ..bond.clone()
        };
        self.save(&mut current, next)
    }

    fn revoke(&self, generation: Uuid) -> Result<ControllerBond, BondError> {
        let mut current = self.bond.lock().map_err(|_| BondError::Unavailable)?;
        let bond = current.as_ref().ok_or(BondError::Unavailable)?;
        if bond.generation != generation {
            return Err(BondError::Conflict);
        }
        let next = ControllerBond {
            generation: Uuid::new_v4(),
            controller: None,
            ..bond.clone()
        };
        self.save(&mut current, next)
    }

    fn authorized(
        &self,
        engine: &Engine,
        requester: DeviceId,
    ) -> Result<ControllerBond, BondError> {
        let current = crate::sync_control::current_identity(engine, requester)
            .map_err(|_| BondError::Rejected)?;
        let bond = self.snapshot()?;
        if bond.appliance != engine.public_identity() || bond.controller.as_ref() != Some(&current)
        {
            return Err(BondError::Rejected);
        }
        Ok(bond)
    }

    pub(crate) async fn read_status(
        &self,
        engine: &Engine,
        requester: DeviceId,
        service: Option<&crate::sync_engine::FolderSyncService>,
    ) -> Result<ApplianceStatus, crate::sync_control::FolderControlError> {
        use crate::sync_control::FolderControlError;
        use crate::sync_engine::{FolderHealthFreshness, FolderSyncLifecycle};
        let before = self
            .authorized(engine, requester)
            .map_err(|_| FolderControlError::Rejected)?;
        let status = match service {
            Some(service) => service.status().await.ok(),
            None => None,
        };
        let after = self
            .authorized(engine, requester)
            .map_err(|_| FolderControlError::Rejected)?;
        if before != after {
            return Err(FolderControlError::Rejected);
        }
        Ok(ApplianceStatus {
            schema_version: 1,
            appliance_id: engine.device_id(),
            controller_id: requester,
            bond_generation: after.generation,
            sync_available: status.is_some(),
            sync_lifecycle: status
                .as_ref()
                .map_or(ApplianceSyncLifecycle::Unavailable, |status| {
                    match status.lifecycle() {
                        FolderSyncLifecycle::Stopped => ApplianceSyncLifecycle::Stopped,
                        FolderSyncLifecycle::InitialScanning => {
                            ApplianceSyncLifecycle::InitialScanning
                        }
                        FolderSyncLifecycle::Running => ApplianceSyncLifecycle::Running,
                        FolderSyncLifecycle::StillStopping => ApplianceSyncLifecycle::StillStopping,
                        FolderSyncLifecycle::NeedsAttention(_) => {
                            ApplianceSyncLifecycle::NeedsAttention
                        }
                    }
                }),
            share_records: status.as_ref().map(|status| status.shares().len() as u64),
            folder_health_fresh: status
                .as_ref()
                .is_some_and(|status| status.health_freshness() == FolderHealthFreshness::Fresh),
            cloud_backup_verification_available: false,
            source_cleanup_available: false,
        })
    }
}

impl ControllerBondStore {
    pub(crate) async fn read_platform(
        &self,
        engine: &Engine,
        requester: DeviceId,
    ) -> Result<AppliancePlatformStatus, crate::sync_control::FolderControlError> {
        use crate::sync_control::FolderControlError;
        let before = self
            .authorized(engine, requester)
            .map_err(|_| FolderControlError::Rejected)?;
        let _permit = self
            .platform_reads
            .clone()
            .try_acquire_owned()
            .map_err(|_| FolderControlError::Busy)?;
        let snapshot = match self.platform.get() {
            None => None,
            Some(reader) => {
                match tokio::time::timeout(std::time::Duration::from_secs(8), reader.read()).await {
                    Ok(Ok(value)) if value.validate().is_ok() => Some(value),
                    _ => None,
                }
            }
        };
        if self
            .authorized(engine, requester)
            .map_err(|_| FolderControlError::Rejected)?
            != before
        {
            return Err(FolderControlError::Rejected);
        }
        Ok(AppliancePlatformStatus {
            schema_version: 1,
            appliance_id: engine.device_id(),
            controller_id: requester,
            bond_generation: before.generation,
            platform_supported: self.platform.get().is_some(),
            platform_available: snapshot.is_some(),
            snapshot,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppliancePlatformStatus {
    schema_version: u16,
    appliance_id: DeviceId,
    controller_id: DeviceId,
    bond_generation: Uuid,
    platform_supported: bool,
    platform_available: bool,
    #[serde(deserialize_with = "required_nullable")]
    snapshot: Option<crate::appliance_platform::PlatformSnapshot>,
}

impl AppliancePlatformStatus {
    fn matches_request(&self, appliance: DeviceId, controller: DeviceId) -> bool {
        self.schema_version == 1
            && self.appliance_id == appliance
            && self.controller_id == controller
            && !self.bond_generation.is_nil()
            && (self.platform_supported || !self.platform_available)
            && self.platform_available == self.snapshot.is_some()
            && self
                .snapshot
                .as_ref()
                .is_none_or(|value| value.validate().is_ok())
    }
}

/// Read-only control v1. It never claims cloud backup, transfer completion or deletion authority.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplianceStatus {
    pub schema_version: u16,
    pub appliance_id: DeviceId,
    pub controller_id: DeviceId,
    pub bond_generation: Uuid,
    pub sync_available: bool,
    pub sync_lifecycle: ApplianceSyncLifecycle,
    #[serde(deserialize_with = "required_nullable")]
    pub share_records: Option<u64>,
    pub folder_health_fresh: bool,
    pub cloud_backup_verification_available: bool,
    pub source_cleanup_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum ApplianceSyncLifecycle {
    Unavailable,
    Stopped,
    InitialScanning,
    Running,
    StillStopping,
    NeedsAttention,
}

impl ApplianceStatus {
    fn matches_request(&self, appliance: DeviceId, controller: DeviceId) -> bool {
        self.schema_version == 1
            && self.appliance_id == appliance
            && self.controller_id == controller
            && !self.bond_generation.is_nil()
            && !self.cloud_backup_verification_available
            && !self.source_cleanup_available
            && if self.sync_available {
                self.sync_lifecycle != ApplianceSyncLifecycle::Unavailable
                    && self.share_records.is_some()
            } else {
                self.sync_lifecycle == ApplianceSyncLifecycle::Unavailable
                    && self.share_records.is_none()
                    && !self.folder_health_fresh
            }
    }
}

fn error(reason: BondError) -> ApiError {
    let (status, code, message) = match reason {
        BondError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "controller_unavailable",
            "Controller bond needs local attention; saved state is retained.",
        ),
        BondError::Rejected => (
            StatusCode::FORBIDDEN,
            "controller_rejected",
            "Explicit owner approval of this paired identity is required.",
        ),
        BondError::Conflict => (
            StatusCode::CONFLICT,
            "controller_conflict",
            "The controller bond changed; refresh before confirming. Revoke an existing controller before replacing it.",
        ),
    };
    ApiError {
        status,
        code,
        message,
        retryable: false,
        upload_offset: None,
    }
}

fn store(state: &AppState) -> Result<&ControllerBondStore, ApiError> {
    state
        .controller_bond
        .as_deref()
        .ok_or_else(|| error(BondError::Unavailable))
}

pub(crate) async fn bond_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    authorize(&state, &headers)?;
    let bond = store(&state)?.snapshot().map_err(error)?;
    let config = state
        .engine
        .config()
        .map_err(|_| error(BondError::Unavailable))?;
    let peers = config
        .trusted_peers
        .keys()
        .filter_map(|peer| crate::sync_control::current_identity(&state.engine, *peer).ok())
        .collect::<Vec<_>>();
    Ok(axum::Json(
        serde_json::json!({"bond": bond, "pairedDevices": peers}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GrantRequest {
    generation: Uuid,
    controller: PublicIdentity,
}

pub(crate) async fn grant_controller(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<GrantRequest>,
) -> Result<axum::Json<ControllerBond>, ApiError> {
    authorize(&state, &headers)?;
    Ok(axum::Json(
        store(&state)?
            .grant(&state.engine, request.generation, request.controller)
            .map_err(error)?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RevokeRequest {
    generation: Uuid,
}

pub(crate) async fn revoke_controller(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RevokeRequest>,
) -> Result<axum::Json<ControllerBond>, ApiError> {
    authorize(&state, &headers)?;
    Ok(axum::Json(
        store(&state)?.revoke(request.generation).map_err(error)?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RemoteStatusRequest {
    peer_id: DeviceId,
}

pub(crate) async fn remote_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RemoteStatusRequest>,
) -> Result<axum::Json<ApplianceStatus>, ApiError> {
    use crate::sync_control::{FolderControlOperation, FolderControlPayload, send_folder_control};
    authorize(&state, &headers)?;
    let config = state
        .engine
        .config()
        .map_err(|_| error(BondError::Unavailable))?;
    let binding = config
        .trusted_peer_transports
        .get(&request.peer_id)
        .cloned()
        .ok_or_else(|| error(BondError::Rejected))?;
    let result = send_folder_control(
        std::sync::Arc::clone(&state.engine),
        binding,
        FolderControlOperation::ReadApplianceStatus {
            requester_id: state.engine.device_id(),
            target_id: request.peer_id,
        },
    )
    .await
    .map_err(|_| error(BondError::Unavailable))?;
    match result {
        FolderControlPayload::ApplianceStatus(status)
            if status.matches_request(request.peer_id, state.engine.device_id()) =>
        {
            Ok(axum::Json(status))
        }
        FolderControlPayload::Rejected => Err(error(BondError::Rejected)),
        _ => Err(error(BondError::Unavailable)),
    }
}

pub(crate) async fn remote_platform(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RemoteStatusRequest>,
) -> Result<axum::Json<AppliancePlatformStatus>, ApiError> {
    use crate::sync_control::{FolderControlOperation, FolderControlPayload, send_folder_control};
    authorize(&state, &headers)?;
    let binding = state
        .engine
        .config()
        .map_err(|_| error(BondError::Unavailable))?
        .trusted_peer_transports
        .get(&request.peer_id)
        .cloned()
        .ok_or_else(|| error(BondError::Rejected))?;
    let result = send_folder_control(
        Arc::clone(&state.engine),
        binding,
        FolderControlOperation::ReadAppliancePlatform {
            requester_id: state.engine.device_id(),
            target_id: request.peer_id,
        },
    )
    .await
    .map_err(|_| error(BondError::Unavailable))?;
    match result {
        FolderControlPayload::AppliancePlatform(value)
            if value.matches_request(request.peer_id, state.engine.device_id()) =>
        {
            Ok(axum::Json(value))
        }
        FolderControlPayload::Rejected => Err(error(BondError::Rejected)),
        _ => Err(error(BondError::Unavailable)),
    }
}

#[cfg(test)]
mod tests;

mod imports;
mod updates;
pub use imports::ImportStatus;
pub(crate) use imports::{
    check_import, confirm_import, import_actions, import_context, withdraw_import,
};
pub use updates::UpdateStatus;
pub(crate) use updates::{check_update, confirm_update, update_actions, update_context};

fn target(
    state: &AppState,
    peer: DeviceId,
) -> Result<covalent_protocol::TransportBinding, ApiError> {
    state
        .engine
        .config()
        .map_err(|_| error(BondError::Unavailable))?
        .trusted_peer_transports
        .get(&peer)
        .cloned()
        .ok_or_else(|| error(BondError::Rejected))
}
