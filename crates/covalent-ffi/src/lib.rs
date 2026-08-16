//! Narrow binding-safe service façade for Swift, Kotlin/JNI, and future UniFFI generation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use covalent_core::{
    AuthorizedRoot, BackupOptions, Engine, EngineOptions, JobControl, PairingConfirmation,
    PairingSession, RestoreOptions, RestorePlan,
};
use covalent_protocol::{
    BackupId, ConflictPolicy, DeviceId, PROTOCOL_VERSION, PairingInvitation, PeerRole,
    RelativePath, ReplicaAvailability, ReplicaIntent,
};
use serde::{Deserialize, Serialize};

/// Stable service object. `new` supports validation-only clients; `open` enables real workflows.
#[derive(Clone)]
pub struct CovalentService {
    engine: Option<Arc<Engine>>,
    jobs: Option<Arc<Mutex<BTreeMap<String, JobControl>>>>,
}

impl CovalentService {
    /// Creates a validation-only façade with no durable state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            engine: None,
            jobs: None,
        }
    }

    /// Opens the real shared engine for an app-owned durable directory.
    pub fn open(
        data_directory: impl Into<PathBuf>,
        initial_device_name: impl Into<String>,
        initial_lan_discovery_enabled: bool,
    ) -> Result<Self, ServiceError> {
        let mut options = EngineOptions::new(data_directory);
        options.initial_device_name = initial_device_name.into();
        options.initial_lan_discovery_enabled = initial_lan_discovery_enabled;
        let engine = Engine::open(options).map_err(|error| ServiceError::from_engine(&error))?;
        Ok(Self {
            engine: Some(Arc::new(engine)),
            jobs: Some(Arc::new(Mutex::new(BTreeMap::new()))),
        })
    }

    /// Returns the service contract version.
    #[must_use]
    pub const fn protocol_version(&self) -> u16 {
        PROTOCOL_VERSION
    }

    /// Validates a restore destination using the shared engine rules.
    pub fn validate_restore_destination(
        &self,
        authorized_root: impl AsRef<Path>,
        relative_path: &str,
    ) -> Result<RestoreDestination, ServiceError> {
        let root = AuthorizedRoot::open(authorized_root)
            .map_err(|error| ServiceError::from_engine(&error))?;
        let relative = RelativePath::new(relative_path)
            .map_err(|error| ServiceError::new("invalid_relative_path", error.to_string()))?;
        let destination = root
            .resolve(&relative)
            .map_err(|error| ServiceError::from_engine(&error))?;
        Ok(RestoreDestination {
            relative_path: relative.to_string(),
            resolved_path: destination.to_string_lossy().into_owned(),
        })
    }

    /// Returns non-secret config and identity status as stable JSON.
    pub fn status_json(&self) -> Result<String, ServiceError> {
        let engine = self.engine()?;
        let config = engine
            .config()
            .map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&ServiceStatus {
            protocol_version: PROTOCOL_VERSION,
            device_id: engine.device_id().to_string(),
            device_name: config.device_name,
            lan_discovery_enabled: config.lan_discovery_enabled,
            remembered_backups: config.remembered_backups.len(),
            trusted_peers: config
                .trusted_peers
                .values()
                .filter(|grant| !grant.revoked)
                .count(),
        })
    }

    /// Exports the safe settings contract as JSON.
    pub fn export_settings_json(&self) -> Result<String, ServiceError> {
        let bytes = self
            .engine()?
            .export_settings()
            .map_err(|error| ServiceError::from_engine(&error))?;
        String::from_utf8(bytes)
            .map_err(|_| ServiceError::new("serialization_failed", "settings were not UTF-8"))
    }

    /// Lists authoritative remembered backups and latest local snapshots as stable JSON.
    pub fn backups_json(&self) -> Result<String, ServiceError> {
        let backups = self
            .engine()?
            .list_backups()
            .map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&backups)
    }

    /// Imports safe settings after an explicit platform confirmation.
    pub fn import_settings_json(
        &self,
        settings_json: &str,
        confirmed: bool,
    ) -> Result<(), ServiceError> {
        self.engine()?
            .import_settings(settings_json.as_bytes(), confirmed)
            .map_err(|error| ServiceError::from_engine(&error))
    }

    /// Runs a resumable encrypted local backup from a binding-safe JSON request.
    pub fn backup_json(&self, request_json: &str) -> Result<String, ServiceError> {
        let request: BackupRequest = deserialize(request_json)?;
        let backup_id = match request.backup_id {
            Some(value) => BackupId::from_str(&value)
                .map_err(|_| ServiceError::new("invalid_backup_id", "backup ID is invalid"))?,
            None => BackupId::new(),
        };
        let providers: Result<Vec<_>, _> = request
            .selected_provider_ids
            .iter()
            .map(|value| {
                DeviceId::from_str(value)
                    .map_err(|_| ServiceError::new("invalid_provider_id", "provider ID is invalid"))
            })
            .collect();
        let mut options = BackupOptions::new(backup_id, request.snapshot_id, request.job_id);
        options.display_name = request.display_name;
        options.created_at_unix_ms = now_unix_ms();
        options.replica_intent = ReplicaIntent::explicit(providers?);
        let control = self.job_control(&options.job_id)?;
        let result = self
            .engine()?
            .backup(request.source_root, &options, &control, |_| {});
        self.finish_job_after(&options.job_id, &result)?;
        let result = result.map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&BackupResponse {
            backup_id: backup_id.to_string(),
            snapshot_id: result.manifest.snapshot_id,
            entries: result.manifest.entries.len(),
            bytes_read: result.progress.bytes_read,
            chunks_stored: result.progress.chunks_stored,
            chunks_deduplicated: result.progress.chunks_deduplicated,
            degraded_failures: result.replication.failures.len(),
        })
    }

    /// Authenticates every local object referenced by a committed snapshot.
    pub fn verify_json(&self, request_json: &str) -> Result<String, ServiceError> {
        let request: SnapshotRequest = deserialize(request_json)?;
        let backup_id = parse_backup_id(&request.backup_id)?;
        if request.repair {
            self.engine()?
                .repair_snapshot(backup_id, &request.snapshot_id)
                .map_err(|error| ServiceError::from_engine(&error))?;
        }
        let (report, provider_availability) = if request.verify_providers {
            let availability = self
                .engine()?
                .verify_snapshot_availability(backup_id, &request.snapshot_id)
                .map_err(|error| ServiceError::from_engine(&error))?;
            (availability.local, availability.providers)
        } else {
            (
                self.engine()?
                    .verify_snapshot(backup_id, &request.snapshot_id)
                    .map_err(|error| ServiceError::from_engine(&error))?,
                BTreeMap::new(),
            )
        };
        let intact = report.is_intact();
        serialize(&VerifyResponse {
            verified: report.verified,
            missing: report.missing,
            corrupt: report.corrupt,
            intact,
            provider_availability,
        })
    }

    /// Creates the exact signed restore preview JSON consumed by `restore_execute_json`.
    pub fn restore_preview_json(&self, request_json: &str) -> Result<String, ServiceError> {
        let request: RestorePreviewRequest = deserialize(request_json)?;
        let plan = self
            .engine()?
            .preview_restore(
                parse_backup_id(&request.backup_id)?,
                &request.snapshot_id,
                request.target_root,
                &RestoreOptions {
                    conflict_policy: request.conflict_policy,
                    selected_paths: Default::default(),
                    job_id: request.job_id,
                },
            )
            .map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&plan)
    }

    /// Executes a signed preview without accepting a widened destination root.
    pub fn restore_execute_json(&self, plan_json: &str) -> Result<String, ServiceError> {
        let plan: RestorePlan = deserialize(plan_json)?;
        let control = self.job_control(&plan.job_id)?;
        let report = self.engine()?.restore(&plan, &control);
        self.finish_job_after(&plan.job_id, &report)?;
        let report = report.map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&RestoreResponse {
            files_restored: report.files_restored,
            directories_created: report.directories_created,
            files_skipped: report.files_skipped,
            bytes_written: report.bytes_written,
            rejected_provider_copies: report.rejected_provider_copies.len(),
        })
    }

    /// Creates a signed expiring pairing invitation as JSON.
    pub fn pairing_invitation_json(
        &self,
        lifetime_ms: u64,
        endpoints: Vec<String>,
    ) -> Result<String, ServiceError> {
        let invitation = self
            .engine()?
            .pairing_manager()
            .create_invitation(now_unix_ms(), lifetime_ms, endpoints)
            .map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&invitation)
    }

    /// Accepts a signed invitation and returns a transferable role-bound exchange.
    pub fn pairing_accept_json(&self, request_json: &str) -> Result<String, ServiceError> {
        let request: PairAcceptRequest = deserialize(request_json)?;
        let session = self
            .engine()?
            .accept_pairing(
                request.invitation,
                request.responder_name,
                request.responder_roles,
                request.inviter_roles,
                now_unix_ms(),
            )
            .map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&session)
    }

    /// Records responder-side explicit short-code confirmation.
    pub fn pairing_confirm_responder_json(
        &self,
        session_json: &str,
        displayed_code: &str,
    ) -> Result<String, ServiceError> {
        let mut session: PairingSession = deserialize(session_json)?;
        self.engine()?
            .confirm_pairing_as_responder(&mut session, displayed_code, now_unix_ms())
            .map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&session)
    }

    /// Records inviter-side explicit short-code confirmation.
    pub fn pairing_confirm_inviter_json(
        &self,
        session_json: &str,
        displayed_code: &str,
    ) -> Result<String, ServiceError> {
        let mut session: PairingSession = deserialize(session_json)?;
        self.engine()?
            .confirm_pairing_as_inviter(&mut session, displayed_code, now_unix_ms())
            .map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&session)
    }

    /// Finalizes and stores the inviter's confirmed responder grant.
    pub fn pairing_finalize_inviter_json(
        &self,
        session_json: &str,
    ) -> Result<String, ServiceError> {
        let session: PairingSession = deserialize(session_json)?;
        let confirmation: PairingConfirmation = self
            .engine()?
            .finalize_pairing_as_inviter(&session, now_unix_ms())
            .map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&confirmation)
    }

    /// Finalizes and stores the responder's confirmed inviter grant.
    pub fn pairing_finalize_responder_json(
        &self,
        session_json: &str,
    ) -> Result<String, ServiceError> {
        let session: PairingSession = deserialize(session_json)?;
        let confirmation: PairingConfirmation = self
            .engine()?
            .finalize_pairing_as_responder(&session, now_unix_ms())
            .map_err(|error| ServiceError::from_engine(&error))?;
        serialize(&confirmation)
    }

    /// Pauses, resumes, or cancels an active binding-safe job by stable ID.
    pub fn control_job(&self, job_id: &str, action: &str) -> Result<(), ServiceError> {
        let control = self.job_control(job_id)?;
        match action {
            "pause" => control.pause(),
            "resume" => control.resume(),
            "cancel" => control.cancel(),
            _ => {
                return Err(ServiceError::new(
                    "invalid_job_action",
                    "job action is invalid",
                ));
            }
        }
        Ok(())
    }

    /// Revokes a paired peer and emits a new signed roster epoch.
    pub fn revoke_peer(&self, peer_id: &str) -> Result<(), ServiceError> {
        let peer_id = DeviceId::from_str(peer_id)
            .map_err(|_| ServiceError::new("invalid_peer_id", "peer ID is invalid"))?;
        self.engine()?
            .revoke_peer(peer_id)
            .map(|_| ())
            .map_err(|error| ServiceError::from_engine(&error))
    }

    fn engine(&self) -> Result<&Engine, ServiceError> {
        self.engine
            .as_deref()
            .ok_or_else(|| ServiceError::new("service_not_open", "open durable state first"))
    }

    fn job_control(&self, job_id: &str) -> Result<JobControl, ServiceError> {
        if job_id.is_empty() || job_id.len() > 128 {
            return Err(ServiceError::new("invalid_job_id", "job ID is invalid"));
        }
        let jobs = self
            .jobs
            .as_ref()
            .ok_or_else(|| ServiceError::new("service_not_open", "open durable state first"))?;
        let mut jobs = jobs
            .lock()
            .map_err(|_| ServiceError::new("engine_failed", "job state is unavailable"))?;
        if jobs.len() >= 1_024 && !jobs.contains_key(job_id) {
            return Err(ServiceError::new("resource_limit", "too many active jobs"));
        }
        Ok(jobs
            .entry(job_id.to_owned())
            .or_insert_with(JobControl::new)
            .clone())
    }

    fn finish_job_after<T>(
        &self,
        job_id: &str,
        result: &Result<T, covalent_core::CoreError>,
    ) -> Result<(), ServiceError> {
        if result.is_ok() || matches!(result, Err(covalent_core::CoreError::Cancelled)) {
            if matches!(result, Err(covalent_core::CoreError::Cancelled)) {
                self.engine()?
                    .discard_job_checkpoint(job_id)
                    .map_err(|error| ServiceError::from_engine(&error))?;
            }
            self.jobs
                .as_ref()
                .ok_or_else(|| ServiceError::new("service_not_open", "open durable state first"))?
                .lock()
                .map_err(|_| ServiceError::new("engine_failed", "job state is unavailable"))?
                .remove(job_id);
        }
        Ok(())
    }
}

impl Default for CovalentService {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CovalentService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CovalentService")
            .field("open", &self.engine.is_some())
            .finish()
    }
}

/// Binding-safe validated restore destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreDestination {
    /// Canonical relative protocol path.
    pub relative_path: String,
    /// Local display path beneath the authorized root.
    pub resolved_path: String,
}

/// Stable binding-safe service error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceError {
    /// Contract version used to interpret this error.
    pub protocol_version: u16,
    /// Stable machine-readable code.
    pub code: String,
    /// Safe local message.
    pub message: String,
    /// Whether retrying the unchanged request may succeed later.
    pub retryable: bool,
}

impl ServiceError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }

    fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            code: code.into(),
            message: message.into(),
            retryable: true,
        }
    }

    fn from_engine(error: &covalent_core::CoreError) -> Self {
        use covalent_core::CoreError;
        match error {
            CoreError::InvalidAuthorizedRoot(_) => Self::new(
                "invalid_authorized_root",
                "The selected source or destination is not an accessible directory.",
            ),
            CoreError::SymlinkTraversal(_)
            | CoreError::NonDirectoryAncestor(_)
            | CoreError::EscapedAuthorizedRoot(_) => Self::new(
                "unsafe_restore_path",
                "The restore path cannot be confined beneath the authorized destination.",
            ),
            CoreError::SettingsImportNotConfirmed | CoreError::PairingNotConfirmed => Self::new(
                "confirmation_required",
                "Explicit local confirmation is required.",
            ),
            CoreError::Paused => Self::new(
                "job_paused",
                "The job is paused and can be resumed with the same job ID.",
            ),
            CoreError::Cancelled => Self::new(
                "job_cancelled",
                "The job was cancelled and its checkpoint was discarded.",
            ),
            CoreError::RestoreConflict(_) => Self::new(
                "restore_conflict",
                "The restore preview found a destination conflict.",
            ),
            CoreError::RestorePlanMismatch => Self::new(
                "restore_plan_mismatch",
                "The restore plan changed after preview. Preview the restore again.",
            ),
            CoreError::InvitationUnavailable => Self::new(
                "invitation_unavailable",
                "The pairing invitation is invalid, expired, or already used.",
            ),
            CoreError::ProtocolNegotiationFailed => Self::new(
                "protocol_incompatible",
                "The devices do not share a supported protocol version.",
            ),
            CoreError::SourceChanged(_) => Self::retryable(
                "source_changed",
                "The source changed while it was being backed up. Retry after writes stop.",
            ),
            CoreError::UnsupportedSourceEntry(_) | CoreError::SourcePermissionDenied(_) => {
                Self::new(
                    "source_unreadable",
                    "The source contains an unsupported or unreadable entry.",
                )
            }
            CoreError::CorruptChunk(_) | CoreError::AuthenticationFailed => Self::new(
                "backup_corrupt",
                "Backup data failed authenticated integrity verification.",
            ),
            CoreError::MissingChunk(_) | CoreError::ProvidersExhausted(_) => Self::retryable(
                "backup_unavailable",
                "No intact authorized copy is currently available.",
            ),
            CoreError::ResourceLimit(_) | CoreError::SettingsTooLarge => Self::new(
                "resource_limit",
                "The request exceeded a configured resource limit.",
            ),
            CoreError::PeerRevoked
            | CoreError::UnselectedProvider
            | CoreError::IdentityMismatch => Self::new(
                "not_authorized",
                "The requested peer or provider is not authorized.",
            ),
            CoreError::InvalidKeyMaterial
            | CoreError::UnsupportedCipherSuite(_)
            | CoreError::InvalidState(_)
            | CoreError::InvalidLocator
            | CoreError::Contract(_)
            | CoreError::Json(_) => Self::new(
                "invalid_contract",
                "The request does not satisfy the versioned protocol contract.",
            ),
            CoreError::StateLocked => Self::retryable(
                "node_state_locked",
                "Another Covalent process currently owns this node state.",
            ),
            CoreError::Synchronization | CoreError::Io { .. } => Self::retryable(
                "engine_failed",
                "The local engine could not complete the request.",
            ),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceStatus {
    protocol_version: u16,
    device_id: String,
    device_name: String,
    lan_discovery_enabled: bool,
    remembered_backups: usize,
    trusted_peers: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupRequest {
    source_root: PathBuf,
    backup_id: Option<String>,
    display_name: String,
    snapshot_id: String,
    job_id: String,
    #[serde(default)]
    selected_provider_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupResponse {
    backup_id: String,
    snapshot_id: String,
    entries: usize,
    bytes_read: u64,
    chunks_stored: usize,
    chunks_deduplicated: usize,
    degraded_failures: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotRequest {
    backup_id: String,
    snapshot_id: String,
    #[serde(default)]
    verify_providers: bool,
    #[serde(default)]
    repair: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyResponse {
    verified: usize,
    missing: Vec<String>,
    corrupt: Vec<String>,
    intact: bool,
    provider_availability: BTreeMap<DeviceId, ReplicaAvailability>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestorePreviewRequest {
    backup_id: String,
    snapshot_id: String,
    target_root: PathBuf,
    conflict_policy: ConflictPolicy,
    job_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreResponse {
    files_restored: usize,
    directories_created: usize,
    files_skipped: usize,
    bytes_written: u64,
    rejected_provider_copies: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairAcceptRequest {
    invitation: PairingInvitation,
    responder_name: String,
    #[serde(default)]
    responder_roles: BTreeSet<PeerRole>,
    #[serde(default)]
    inviter_roles: BTreeSet<PeerRole>,
}

fn parse_backup_id(value: &str) -> Result<BackupId, ServiceError> {
    BackupId::from_str(value)
        .map_err(|_| ServiceError::new("invalid_backup_id", "backup ID is invalid"))
}

fn serialize(value: &impl Serialize) -> Result<String, ServiceError> {
    serde_json::to_string(value)
        .map_err(|_| ServiceError::new("serialization_failed", "could not encode JSON"))
}

fn deserialize<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, ServiceError> {
    serde_json::from_str(value)
        .map_err(|_| ServiceError::new("invalid_request", "request JSON does not match contract"))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn facade_uses_shared_restore_validation() {
        let root = tempdir().expect("temporary root");
        let service = CovalentService::new();
        assert!(
            service
                .validate_restore_destination(root.path(), "nested/file.txt")
                .is_ok()
        );
        assert_eq!(
            service
                .validate_restore_destination(root.path(), "../escape")
                .expect_err("traversal must fail")
                .code,
            "invalid_relative_path"
        );
    }

    #[test]
    fn binding_error_matches_the_shared_versioned_fixture() {
        let expected: ServiceError =
            serde_json::from_str(include_str!("../../../fixtures/contracts/error-v1.json"))
                .expect("error fixture");
        let actual = ServiceError::from_engine(&covalent_core::CoreError::SourceChanged(
            PathBuf::from("private-source-name"),
        ));
        assert_eq!(actual, expected);
        assert!(!actual.message.contains("private-source-name"));
    }

    #[test]
    fn stateful_json_facade_executes_backup_verify_and_restore() {
        let data = tempdir().expect("data");
        let source = tempdir().expect("source");
        let restore = tempdir().expect("restore");
        fs::write(source.path().join("file.txt"), b"ffi content").expect("source");
        let service = CovalentService::open(data.path(), "Native app", false).expect("service");
        let request = serde_json::json!({
            "sourceRoot": source.path(),
            "displayName": "Files",
            "snapshotId": "snapshot-1",
            "jobId": "backup-job",
            "selectedProviderIds": []
        });
        let backup: serde_json::Value =
            serde_json::from_str(&service.backup_json(&request.to_string()).expect("backup"))
                .expect("backup response");
        let backup_id = backup["backupId"].as_str().expect("backup ID");
        let backups: serde_json::Value =
            serde_json::from_str(&service.backups_json().expect("backup list"))
                .expect("backup list response");
        assert_eq!(backups[0]["backupId"], backup_id);
        assert_eq!(backups[0]["latestSnapshotId"], "snapshot-1");
        let verify = serde_json::json!({
            "backupId": backup_id,
            "snapshotId": "snapshot-1"
        });
        let verified: serde_json::Value =
            serde_json::from_str(&service.verify_json(&verify.to_string()).expect("verify"))
                .expect("verify response");
        assert_eq!(verified["intact"], true);
        let preview = serde_json::json!({
            "backupId": backup_id,
            "snapshotId": "snapshot-1",
            "targetRoot": restore.path(),
            "conflictPolicy": "fail",
            "jobId": "restore-job"
        });
        let plan = service
            .restore_preview_json(&preview.to_string())
            .expect("preview");
        service.restore_execute_json(&plan).expect("restore");
        assert_eq!(
            fs::read(restore.path().join("file.txt")).expect("restored"),
            b"ffi content"
        );
    }
}
