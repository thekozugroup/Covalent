//! Exact rclone process inputs and durable one-way deletion policy state.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use caseless::Caseless as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use unicode_normalization::UnicodeNormalization as _;
use uuid::Uuid;

use super::android_saf::{AndroidSafGrantError, AndroidSafGrantRegistry};
use super::config::{EngineDeviceId, EngineFolderRole};
use super::controller::{EngineSessionSettings, FolderRootLease};
use super::installation::{EngineInstallation, EngineWorkerLease};
use super::supervisor::OwnedRcloneCommand;
use super::{EngineIndexSnapshot, EngineSessionError, VerifiedEngineExecutable};

const MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_POLICY_BYTES: u64 = 64 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const MAX_UNRESOLVED_TRANSFERS: usize = 512;
const MAX_RECOVERY_READ_BYTES: u64 = 8 * 1024 * 1024;

/// Persisted uncertainty only; this never establishes cloud backup or consent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRecoverySummary {
    pub folder_id: Uuid,
    pub available: bool,
    pub unconfirmed_files: Option<u64>,
    pub unconfirmed_source_bytes: Option<u64>,
}

pub(super) struct RecoveryReader {
    installation: Arc<EngineInstallation>,
    folders: BTreeSet<Uuid>,
    active: BTreeSet<Uuid>,
}

impl RecoveryReader {
    pub(super) fn new(
        installation: Arc<EngineInstallation>,
        folders: BTreeSet<Uuid>,
        active: BTreeSet<Uuid>,
    ) -> Self {
        Self {
            installation,
            folders,
            active,
        }
    }

    pub(super) fn read(self) -> Vec<FolderRecoverySummary> {
        let directory = self.installation.database_directory().ok();
        let mut remaining = MAX_RECOVERY_READ_BYTES;
        self.folders
            .into_iter()
            .map(|folder_id| {
                let observed = (|| {
                    if remaining == 0 {
                        return Err(EngineSessionError::RuntimeUnavailable);
                    }
                    let directory = directory
                        .as_ref()
                        .ok_or(EngineSessionError::RuntimeUnavailable)?;
                    let (path, staged) = policy_paths(directory, folder_id);
                    let (loaded, bytes) = load_policy_counted(&path, &staged, remaining);
                    remaining = remaining.saturating_sub(bytes);
                    let policy = loaded?;
                    let Some(state) = policy.folders.get(&folder_id) else {
                        return if policy.folders.is_empty() {
                            Ok((0, 0))
                        } else {
                            Err(EngineSessionError::RuntimeUnavailable)
                        };
                    };
                    unconfirmed_counts(state, self.active.contains(&folder_id))
                })();
                match observed {
                    Ok((files, bytes)) => FolderRecoverySummary {
                        folder_id,
                        available: true,
                        unconfirmed_files: Some(files),
                        unconfirmed_source_bytes: Some(bytes),
                    },
                    Err(_) => FolderRecoverySummary {
                        folder_id,
                        available: false,
                        unconfirmed_files: None,
                        unconfirmed_source_bytes: None,
                    },
                }
            })
            .collect()
    }
}

// Embedded Android has no tracing subscriber. Its JNI boundary installs the
// platform log sink; the node crate keeps its existing unsafe-code prohibition.
static TRANSFER_DIAGNOSTIC_SINK: OnceLock<fn(&str)> = OnceLock::new();

pub fn set_transfer_diagnostic_sink(sink: fn(&str)) {
    let _ = TRANSFER_DIAGNOSTIC_SINK.set(sink);
}

pub(super) fn report_transfer_error(operation: &str, error: EngineSessionError) {
    emit_transfer_diagnostic(operation, &format!("{error:?}"), None, &[]);
}

fn emit_transfer_diagnostic(
    operation: &str,
    category: &str,
    exit_code: Option<i32>,
    stderr: &[u8],
) {
    let message = transfer_diagnostic_message(operation, category, exit_code, stderr);
    if let Some(sink) = TRANSFER_DIAGNOSTIC_SINK.get() {
        sink(&message);
    } else {
        tracing::warn!(target: "covalent_sync_diagnostic", "{message}");
    }
}

fn transfer_diagnostic_message(
    operation: &str,
    category: &str,
    exit_code: Option<i32>,
    stderr: &[u8],
) -> String {
    let operation = match operation {
        "lsjson" | "copy" | "sync" | "check" | "destination" | "source-index"
        | "source-after-copy" | "target-verify" => operation,
        _ => "other",
    };
    let category = match category {
        "InvalidConfiguration"
        | "RuntimeUnavailable"
        | "FolderUnavailable"
        | "LaunchFailed"
        | "StartupTimeout"
        | "StartupMismatch"
        | "EngineUnavailable"
        | "TransferFailed"
        | "PendingCopyRecoveryRequired"
        | "NonzeroExit"
        | "TimedOut"
        | "WaitFailed" => category,
        _ => "Unknown",
    };
    let mut causes = BTreeSet::new();
    // Inspect only a bounded stderr tail. JSON object/path fields are never
    // emitted. Plain-text check/list errors use the same fixed classifier.
    for line in stderr[stderr.len().saturating_sub(65_536)..]
        .rsplit(|byte| *byte == b'\n')
        .take(256)
    {
        if line.len() > 8192 {
            continue;
        }
        if let Ok(record) = serde_json::from_slice::<serde_json::Value>(line) {
            if record
                .get("level")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|level| !matches!(level, "error" | "fatal" | "warning"))
            {
                continue;
            }
            for field in ["msg", "error"] {
                if let Some(message) = record.get(field).and_then(serde_json::Value::as_str) {
                    classify_transfer_cause(message, &mut causes);
                }
            }
        } else if let Ok(message) = std::str::from_utf8(line) {
            classify_transfer_cause(message, &mut causes);
        }
    }
    let causes = if causes.is_empty() {
        "unclassified".to_owned()
    } else {
        causes.into_iter().collect::<Vec<_>>().join(",")
    };
    let exit_code = exit_code.map_or_else(|| "none".to_owned(), |value| value.to_string());
    format!(
        "COVALENT_SYNC_DIAGNOSTIC operation={operation} category={category} exitCode={exit_code} causes={causes}"
    )
}

fn classify_transfer_cause(message: &str, causes: &mut BTreeSet<&'static str>) {
    let message = message.to_ascii_lowercase();
    for (category, patterns) in [
        (
            "permission",
            &[
                "permission denied",
                "access denied",
                "forbidden",
                "unauthorized",
            ][..],
        ),
        ("readOnly", &["read-only", "read only"][..]),
        (
            "notFound",
            &["not found", "no such file", "does not exist"][..],
        ),
        (
            "timeout",
            &["timeout", "timed out", "deadline exceeded"][..],
        ),
        (
            "network",
            &[
                "connection refused",
                "connection reset",
                "broken pipe",
                "network is unreachable",
                "no route to host",
                "dial tcp",
                "unexpected eof",
                "closed network connection",
            ][..],
        ),
        (
            "unsupported",
            &[
                "not implemented",
                "method not allowed",
                "unsupported",
                "not supported",
            ][..],
        ),
        (
            "integrityMismatch",
            &[
                "size differ",
                "sizes differ",
                "size mismatch",
                "size changed",
                "hash differ",
                "hashes differ",
                "hash mismatch",
                "checksum mismatch",
                "corrupted on transfer",
            ][..],
        ),
        (
            "noSpace",
            &["no space left", "disk full", "insufficient storage"][..],
        ),
        (
            "conflict",
            &["locked", "conflict", "precondition failed"][..],
        ),
        ("rateLimited", &["too many requests"][..]),
    ] {
        if patterns.iter().any(|pattern| message.contains(pattern)) {
            causes.insert(category);
        }
    }
    for (code, reason, category) in [
        (400, "bad request", "http400"),
        (401, "unauthorized", "http401"),
        (403, "forbidden", "http403"),
        (404, "not found", "http404"),
        (405, "method not allowed", "http405"),
        (408, "request timeout", "http408"),
        (409, "conflict", "http409"),
        (412, "precondition failed", "http412"),
        (423, "locked", "http423"),
        (429, "too many requests", "http429"),
        (500, "internal server error", "http500"),
        (501, "not implemented", "http501"),
        (502, "bad gateway", "http502"),
        (503, "service unavailable", "http503"),
        (504, "gateway timeout", "http504"),
        (507, "insufficient storage", "http507"),
    ] {
        if message.contains(&format!("{code} {reason}"))
            || message.contains(&format!("status code: {code}"))
            || message.contains(&format!("status code {code}"))
        {
            causes.insert(category);
        }
    }
}

#[derive(Clone)]
pub(super) enum FolderBackend {
    Local(PathBuf),
    WebDav {
        grant_id: Uuid,
        address: SocketAddr,
        username: String,
        obscured_password: String,
    },
}

impl std::fmt::Debug for FolderBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(_) => formatter.write_str("FolderBackend::Local([PRIVATE])"),
            Self::WebDav { grant_id, .. } => formatter
                .debug_struct("FolderBackend::WebDav")
                .field("grant_id", grant_id)
                .field("capability", &"[PRIVATE]")
                .finish(),
        }
    }
}

#[derive(Clone)]
pub(super) struct RcloneFolder {
    pub id: Uuid,
    pub role: EngineFolderRole,
    pub backend: FolderBackend,
    pub members: Vec<EngineDeviceId>,
}

#[derive(Clone)]
pub(super) struct RclonePeer {
    pub id: EngineDeviceId,
    pub address: SocketAddr,
    pub ssh_public_key: String,
}

type CommandInputs = (Vec<OsString>, Vec<(OsString, OsString)>);

struct GrantReservations {
    registry: AndroidSafGrantRegistry,
    grants: BTreeSet<Uuid>,
}

impl GrantReservations {
    fn reserve(&mut self, grant: Uuid) -> Result<(), EngineSessionError> {
        if !self.grants.contains(&grant) {
            self.registry.mark_active(grant).map_err(map_grant_error)?;
            self.grants.insert(grant);
        }
        Ok(())
    }
}

impl Drop for GrantReservations {
    fn drop(&mut self) {
        for grant in &self.grants {
            self.registry.mark_inactive(*grant);
        }
    }
}

pub(super) struct RcloneRuntime {
    guardian: Arc<VerifiedEngineExecutable>,
    executable: Arc<VerifiedEngineExecutable>,
    runtime: Arc<tempfile::TempDir>,
    key_file: PathBuf,
    known_hosts: BTreeMap<EngineDeviceId, PathBuf>,
    folders: BTreeMap<Uuid, RcloneFolder>,
    peers: BTreeMap<EngineDeviceId, RclonePeer>,
    policy_directory: PathBuf,
    installation: Arc<EngineInstallation>,
    roots: Arc<Vec<FolderRootLease>>,
    worker_lease: Weak<EngineWorkerLease>,
    grants: AndroidSafGrantRegistry,
    active_grants: BTreeSet<Uuid>,
}

impl std::fmt::Debug for RcloneRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RcloneRuntime([PRIVATE])")
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CapabilityMap {
    version: u8,
    entries: Vec<Capability>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capability {
    username: String,
    public_key: String,
    backend: CapabilityBackend,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
struct CapabilityBackend {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(rename = "_root")]
    root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vendor: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pass: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct InventoryEntry {
    pub path: String,
    pub size: i64,
    pub modified_unix_seconds: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListedEntry {
    path: String,
    size: i64,
    mod_time: String,
    #[serde(default)]
    hashes: BTreeMap<String, String>,
    #[serde(default)]
    is_dir: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingTransfer {
    source_index: EngineIndexSnapshot,
    source: BTreeMap<String, InventoryEntry>,
    allowed: BTreeSet<String>,
    suppressions: BTreeSet<String>,
    propagate_source_deletions: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FolderPolicyState {
    #[serde(default)]
    owned: BTreeSet<String>,
    delivered: BTreeMap<String, InventoryEntry>,
    suppressions: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingTransfer>,
    #[serde(default)]
    copy_completed: bool,
    // Preserve the original admitted inventory and uncertain write scope. These
    // paths are held for review; later unrelated files can still be delivered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unresolved: Vec<PendingTransfer>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PolicyState {
    version: u8,
    folders: BTreeMap<Uuid, FolderPolicyState>,
}

enum RcloneCommandFailure {
    Uncertain(EngineSessionError),
    ReapedNonzero { stderr: Vec<u8> },
}

impl RcloneCommandFailure {
    fn into_session_error(self) -> EngineSessionError {
        match self {
            Self::Uncertain(error) => error,
            Self::ReapedNonzero { .. } => EngineSessionError::EngineUnavailable,
        }
    }
}

impl RcloneRuntime {
    pub(super) fn executable(&self) -> &VerifiedEngineExecutable {
        &self.executable
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn prepare(
        guardian: Arc<VerifiedEngineExecutable>,
        executable: Arc<VerifiedEngineExecutable>,
        runtime_parent: &Path,
        settings: &EngineSessionSettings,
        worker_lease: Weak<EngineWorkerLease>,
        installation: Arc<EngineInstallation>,
        roots: Arc<Vec<FolderRootLease>>,
        in_flight: &BTreeSet<Uuid>,
    ) -> Result<Self, EngineSessionError> {
        executable
            .recheck()
            .map_err(|_| EngineSessionError::LaunchFailed)?;
        let policy_directory = installation
            .database_directory()
            .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let runtime = tempfile::Builder::new()
            .prefix("cv-rclone-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(runtime_parent)
            .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let runtime_path =
            fs::canonicalize(runtime.path()).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        if runtime_path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(u8::is_ascii_whitespace)
        {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        let key_file = runtime_path.join("identity.pem");
        write_private_new(
            &key_file,
            installation.identity().private_key_pem().as_bytes(),
            4096,
        )?;
        let grants = executable.android_saf_grants();
        let mut reservations = GrantReservations {
            registry: grants.clone(),
            grants: BTreeSet::new(),
        };
        let mut resolved_folders = BTreeMap::new();
        for folder in &settings.folders {
            if matches!(folder.role(), EngineFolderRole::LegacyTwoWay) {
                return Err(EngineSessionError::InvalidConfiguration);
            }
            let grant_id =
                super::android_saf::parse_token(folder.root()).map_err(map_grant_error)?;
            let resolved = grants.resolve(folder.root()).map_err(map_grant_error)?;
            let backend = if let Some((grant_id, grant)) = resolved {
                reservations.reserve(grant_id)?;
                FolderBackend::WebDav {
                    grant_id,
                    address: grant.address,
                    username: grant.username.to_string(),
                    obscured_password: grant.password.to_string(),
                }
            } else if grant_id.is_none() {
                FolderBackend::Local(folder.root().to_path_buf())
            } else {
                return Err(EngineSessionError::RuntimeUnavailable);
            };
            resolved_folders.insert(
                folder.id(),
                RcloneFolder {
                    id: folder.id(),
                    role: folder.role(),
                    backend,
                    members: folder.members().to_vec(),
                },
            );
        }
        let mut resolved_peers = BTreeMap::new();
        let mut known_hosts = BTreeMap::new();
        for peer in &settings.peers {
            let key = peer
                .ssh_public_key()
                .ok_or(EngineSessionError::InvalidConfiguration)?;
            let known = runtime_path.join(format!("known-host-{}", known_hosts.len()));
            let host = known_host_name(peer.address());
            let line = format!("{host} ssh-ed25519 {key}\n");
            write_private_new(&known, line.as_bytes(), 4096)?;
            known_hosts.insert(peer.id().clone(), known);
            resolved_peers.insert(
                peer.id().clone(),
                RclonePeer {
                    id: peer.id().clone(),
                    address: peer.address(),
                    ssh_public_key: key.to_owned(),
                },
            );
        }
        let proxy = runtime_path.join("auth-proxy");
        symlink(executable.path(), &proxy).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let map_path = runtime_path.join("auth-map.json");
        let map = build_capability_map(&resolved_folders, &resolved_peers)?;
        let map_bytes =
            serde_json::to_vec(&map).map_err(|_| EngineSessionError::InvalidConfiguration)?;
        write_private_new(&map_path, &map_bytes, 1024 * 1024)?;
        for folder_id in resolved_folders.keys().filter(|id| !in_flight.contains(id)) {
            let (policy, staged) = policy_paths(&policy_directory, *folder_id);
            load_policy(&policy, &staged)?;
        }
        // An unchanged in-flight job owns its policy writes. Every new copy still
        // calls load_policy; do not mistake that owner's atomic staging for damage.
        Ok(Self {
            guardian,
            executable,
            runtime: Arc::new(runtime),
            key_file,
            known_hosts,
            folders: resolved_folders,
            peers: resolved_peers,
            policy_directory,
            installation,
            roots,
            worker_lease,
            grants,
            active_grants: std::mem::take(&mut reservations.grants),
        })
    }

    pub(super) fn server_inputs(
        &self,
        listener: SocketAddr,
    ) -> Result<CommandInputs, EngineSessionError> {
        if !self
            .folders
            .values()
            .any(|folder| matches!(folder.role, EngineFolderRole::Source))
        {
            return Err(EngineSessionError::InvalidConfiguration);
        }
        let proxy = self.runtime.path().join("auth-proxy");
        let map = self.runtime.path().join("auth-map.json");
        Ok((
            vec![
                "serve".into(),
                "sftp".into(),
                "--addr".into(),
                listener.to_string().into(),
                "--auth-proxy".into(),
                proxy.into_os_string(),
                "--key".into(),
                self.key_file.clone().into_os_string(),
                "--read-only".into(),
                "--dir-cache-time".into(),
                "0".into(),
                "--stats".into(),
                "0".into(),
            ],
            vec![("COVALENT_RCLONE_AUTH_MAP".into(), map.into_os_string())],
        ))
    }

    pub(super) fn runtime_path(&self) -> &Path {
        self.runtime.path()
    }

    pub(super) async fn initial_scan(&self) -> Result<(), EngineSessionError> {
        for folder in self.folders.values() {
            self.list_backend(&folder.backend, false).await?;
        }
        Ok(())
    }

    pub(super) async fn source_index(
        &self,
        folder_id: Uuid,
    ) -> Result<EngineIndexSnapshot, EngineSessionError> {
        let folder = self
            .folders
            .get(&folder_id)
            .filter(|folder| matches!(folder.role, EngineFolderRole::Source))
            .ok_or(EngineSessionError::InvalidConfiguration)?;
        Ok(inventory_index(
            &self.list_backend(&folder.backend, true).await?,
        ))
    }

    pub(super) async fn transfer_destination(
        &self,
        folder_id: Uuid,
        expected: &EngineIndexSnapshot,
    ) -> Result<EngineIndexSnapshot, EngineSessionError> {
        let (folder, peer) = self.destination_source(folder_id)?;
        let source_entries = self.list_sftp(folder.id, peer).await?;
        let source_index = inventory_index(&source_entries);
        if &source_index != expected {
            report_transfer_error("source-index", EngineSessionError::TransferFailed);
            return Err(EngineSessionError::TransferFailed);
        }
        let source = source_entries
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        let target = self
            .list_backend(&folder.backend, false)
            .await?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        let (policy_path, policy_staged) = policy_paths(&self.policy_directory, folder_id);
        let mut policy = load_policy(&policy_path, &policy_staged)?;
        let state = policy.folders.entry(folder_id).or_default();
        state.owned.extend(state.delivered.keys().cloned());
        let (propagate, restore) = match folder.role {
            EngineFolderRole::Destination {
                propagate_source_deletions,
                restore_local_deletions,
            } => (propagate_source_deletions, restore_local_deletions),
            _ => return Err(EngineSessionError::InvalidConfiguration),
        };
        prepare_pending_retry(state, &target, restore)?;
        hold_unowned_collisions(state, &source, &target, &source_index, propagate, restore)?;
        let held = held_paths(state);
        let (allowed, suppressions) = plan_transfer(state, &source, &target, propagate, restore);
        let pending = PendingTransfer {
            source_index: source_index.clone(),
            source: source.clone(),
            allowed,
            suppressions,
            propagate_source_deletions: propagate,
        };
        if pending.allowed.is_empty() {
            verify_transfer_except(&pending, &target, &held)?;
            complete_transfer_state(state, source, pending.suppressions, propagate);
            let incomplete = !state.unresolved.is_empty();
            save_policy(&policy_path, &policy_staged, &policy)?;
            return if incomplete {
                Err(EngineSessionError::PendingCopyRecoveryRequired)
            } else {
                Ok(source_index)
            };
        }
        state.pending = Some(pending.clone());
        state.copy_completed = false;
        save_policy(&policy_path, &policy_staged, &policy)?;
        let list = self.write_files_list(folder_id, &pending.allowed)?;
        let verified = pending
            .allowed
            .iter()
            .filter(|path| pending.source.contains_key(*path))
            .cloned()
            .collect::<BTreeSet<_>>();
        let verification_list =
            self.write_files_list_with_suffix(folder_id, "verify", &verified)?;
        let result = async {
            self.run_transfer(folder, peer, list.path(), &pending)
                .await?;
            record_completed_copy(&policy_path, &policy_staged, folder_id, &pending)?;
            let after_copy = self
                .list_sftp(folder.id, peer)
                .await?
                .into_iter()
                .map(|entry| (entry.path.clone(), entry))
                .collect::<BTreeMap<_, _>>();
            if !pending_paths_are_unchanged(&pending, &after_copy) {
                report_transfer_error("source-after-copy", EngineSessionError::TransferFailed);
                return Err(EngineSessionError::TransferFailed);
            }
            if verified.is_empty() {
                Ok(())
            } else {
                self.check_transfer(folder, peer, verification_list.path())
                    .await
            }
        }
        .await;
        if let Err(error) = result {
            if error == EngineSessionError::TransferFailed {
                clear_known_pending(&policy_path, &policy_staged, folder_id, &pending)?;
            }
            return Err(error);
        }
        let target_after = self
            .list_backend(&folder.backend, false)
            .await?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        verify_transfer_except(&pending, &target_after, &held).inspect_err(|error| {
            report_transfer_error("target-verify", *error);
        })?;
        let mut next = load_policy(&policy_path, &policy_staged)?;
        let state = next.folders.entry(folder_id).or_default();
        if state
            .pending
            .as_ref()
            .is_none_or(|current| current.source_index != source_index)
        {
            return Err(EngineSessionError::EngineUnavailable);
        }
        complete_transfer_state(state, source, pending.suppressions, propagate);
        let incomplete = !state.unresolved.is_empty();
        save_policy(&policy_path, &policy_staged, &next)?;
        if incomplete {
            Err(EngineSessionError::PendingCopyRecoveryRequired)
        } else {
            Ok(source_index)
        }
    }

    pub(super) fn reset_folder(&self, folder_id: Uuid) -> Result<(), EngineSessionError> {
        let (policy_path, policy_staged) = policy_paths(&self.policy_directory, folder_id);
        let mut policy = load_policy(&policy_path, &policy_staged)?;
        policy.folders.remove(&folder_id);
        save_policy(&policy_path, &policy_staged, &policy)
    }

    fn destination_source(
        &self,
        folder_id: Uuid,
    ) -> Result<(&RcloneFolder, &RclonePeer), EngineSessionError> {
        let folder = self
            .folders
            .get(&folder_id)
            .filter(|folder| matches!(folder.role, EngineFolderRole::Destination { .. }))
            .ok_or(EngineSessionError::InvalidConfiguration)?;
        let peer = folder
            .members
            .iter()
            .find_map(|id| self.peers.get(id))
            .ok_or(EngineSessionError::InvalidConfiguration)?;
        Ok((folder, peer))
    }

    async fn list_backend(
        &self,
        backend: &FolderBackend,
        request_sha256: bool,
    ) -> Result<Vec<InventoryEntry>, EngineSessionError> {
        let (remote, environment) = backend_remote("target", backend);
        self.list(remote, environment, request_sha256).await
    }

    async fn list_sftp(
        &self,
        folder_id: Uuid,
        peer: &RclonePeer,
    ) -> Result<Vec<InventoryEntry>, EngineSessionError> {
        let known = self
            .known_hosts
            .get(&peer.id)
            .ok_or(EngineSessionError::InvalidConfiguration)?;
        let environment = sftp_environment(peer, folder_id, &self.key_file, known);
        self.list(OsString::from("source:"), environment, true)
            .await
    }

    async fn list(
        &self,
        remote: OsString,
        environment: Vec<(OsString, OsString)>,
        request_sha256: bool,
    ) -> Result<Vec<InventoryEntry>, EngineSessionError> {
        let mut args = vec![
            OsString::from("lsjson"),
            remote,
            OsString::from("--recursive"),
            OsString::from("--files-only"),
            OsString::from("--no-mimetype"),
        ];
        if request_sha256 {
            args.extend([
                OsString::from("--hash"),
                OsString::from("--hash-type"),
                OsString::from("SHA-256"),
            ]);
        }
        let bytes = self
            .run(&args, &environment)
            .await
            .map_err(RcloneCommandFailure::into_session_error)?;
        let rows: Vec<ListedEntry> =
            serde_json::from_slice(&bytes).map_err(|_| EngineSessionError::EngineUnavailable)?;
        if rows.len() > 1_000_000 {
            return Err(EngineSessionError::EngineUnavailable);
        }
        let mut result = BTreeMap::new();
        for mut row in rows {
            if row.is_dir || row.path.is_empty() || row.path.contains('\0') || row.size < 0 {
                return Err(EngineSessionError::EngineUnavailable);
            }
            let sha256 = row.hashes.remove("sha256").filter(|value| {
                value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
            let modified_unix_seconds = OffsetDateTime::parse(&row.mod_time, &Rfc3339)
                .map_err(|_| EngineSessionError::EngineUnavailable)?
                .unix_timestamp();
            let entry = InventoryEntry {
                path: row.path,
                size: row.size,
                modified_unix_seconds,
                sha256,
            };
            if result.insert(entry.path.clone(), entry).is_some() {
                return Err(EngineSessionError::EngineUnavailable);
            }
        }
        Ok(result.into_values().collect())
    }

    async fn run_transfer(
        &self,
        folder: &RcloneFolder,
        peer: &RclonePeer,
        files_from: &Path,
        pending: &PendingTransfer,
    ) -> Result<(), EngineSessionError> {
        let mut environment = sftp_environment(
            peer,
            folder.id,
            &self.key_file,
            self.known_hosts
                .get(&peer.id)
                .ok_or(EngineSessionError::InvalidConfiguration)?,
        );
        let (target, target_environment) = backend_remote("target", &folder.backend);
        environment.extend(target_environment);
        let args = vec![
            OsString::from(if pending.propagate_source_deletions {
                "sync"
            } else {
                "copy"
            }),
            OsString::from("source:"),
            target,
            OsString::from("--files-from0"),
            files_from.as_os_str().to_owned(),
            OsString::from("--retries"),
            OsString::from("1"),
            OsString::from("--low-level-retries"),
            OsString::from("1"),
            OsString::from("--stats"),
            OsString::from("0"),
            OsString::from("--no-update-modtime"),
            OsString::from("--ignore-times"),
            OsString::from("--use-json-log"),
            OsString::from("--log-level"),
            OsString::from("INFO"),
        ];
        match self.run(&args, &environment).await {
            Ok(_) => Ok(()),
            Err(RcloneCommandFailure::ReapedNonzero { stderr }) => {
                let (path, staged) = policy_paths(&self.policy_directory, folder.id);
                record_partial_copy(&path, &staged, folder.id, pending, &stderr)?;
                Err(EngineSessionError::EngineUnavailable)
            }
            Err(error) => Err(error.into_session_error()),
        }
    }

    async fn check_transfer(
        &self,
        folder: &RcloneFolder,
        peer: &RclonePeer,
        files_from: &Path,
    ) -> Result<(), EngineSessionError> {
        let mut environment = sftp_environment(
            peer,
            folder.id,
            &self.key_file,
            self.known_hosts
                .get(&peer.id)
                .ok_or(EngineSessionError::InvalidConfiguration)?,
        );
        let (target, target_environment) = backend_remote("target", &folder.backend);
        environment.extend(target_environment);
        let args = vec![
            OsString::from("check"),
            OsString::from("source:"),
            target,
            OsString::from("--download"),
            OsString::from("--one-way"),
            OsString::from("--files-from0"),
            files_from.as_os_str().to_owned(),
            OsString::from("--checkers"),
            OsString::from("4"),
            OsString::from("--stats"),
            OsString::from("0"),
        ];
        self.run(&args, &environment)
            .await
            .map(|_| ())
            .map_err(|error| match error {
                RcloneCommandFailure::ReapedNonzero { .. } => EngineSessionError::TransferFailed,
                RcloneCommandFailure::Uncertain(error) => error,
            })
    }

    fn write_files_list(
        &self,
        folder_id: Uuid,
        files: &BTreeSet<String>,
    ) -> Result<tempfile::NamedTempFile, EngineSessionError> {
        self.write_files_list_with_suffix(folder_id, "transfer", files)
    }

    fn write_files_list_with_suffix(
        &self,
        folder_id: Uuid,
        suffix: &str,
        files: &BTreeSet<String>,
    ) -> Result<tempfile::NamedTempFile, EngineSessionError> {
        write_temporary_files_list(
            self.runtime.path(),
            &format!("files-{folder_id}-{suffix}-"),
            files,
        )
    }

    async fn run(
        &self,
        args: &[OsString],
        environment: &[(OsString, OsString)],
    ) -> Result<Vec<u8>, RcloneCommandFailure> {
        let operation = args
            .first()
            .and_then(|value| value.to_str())
            .unwrap_or("other");
        let mut owned_environment = environment.to_vec();
        owned_environment.extend([
            (OsString::from("RCLONE_CONFIG"), OsString::from("/dev/null")),
            (
                OsString::from("RCLONE_CACHE_DIR"),
                self.runtime.path().join("cache").into_os_string(),
            ),
            (
                OsString::from("RCLONE_TEMP_DIR"),
                self.runtime.path().as_os_str().to_owned(),
            ),
        ]);
        let worker_lease = self
            .worker_lease
            .upgrade()
            .ok_or(RcloneCommandFailure::Uncertain(
                EngineSessionError::LaunchFailed,
            ))?;
        let command = OwnedRcloneCommand::launch(
            &self.guardian,
            &self.executable,
            args,
            &owned_environment,
            self.runtime.path(),
            MAX_OUTPUT_BYTES,
            Box::new((
                Arc::clone(&self.runtime),
                Arc::clone(&self.installation),
                Arc::clone(&self.roots),
                worker_lease,
            )),
        )
        .map_err(|_| {
            emit_transfer_diagnostic(operation, "LaunchFailed", None, &[]);
            RcloneCommandFailure::Uncertain(EngineSessionError::LaunchFailed)
        })?;
        let output = tokio::time::timeout(COMMAND_TIMEOUT, command.wait())
            .await
            .map_err(|_| {
                emit_transfer_diagnostic(operation, "TimedOut", None, &[]);
                RcloneCommandFailure::Uncertain(EngineSessionError::EngineUnavailable)
            })?
            .map_err(|_| {
                emit_transfer_diagnostic(operation, "WaitFailed", None, &[]);
                RcloneCommandFailure::Uncertain(EngineSessionError::EngineUnavailable)
            })?;
        if !output.status.success() {
            emit_transfer_diagnostic(
                operation,
                "NonzeroExit",
                output.status.code(),
                &output.stderr,
            );
            return Err(RcloneCommandFailure::ReapedNonzero {
                stderr: output.stderr,
            });
        }
        let _discarded_stderr = output.stderr;
        Ok(output.stdout)
    }
}

fn write_temporary_files_list(
    directory: &Path,
    prefix: &str,
    files: &BTreeSet<String>,
) -> Result<tempfile::NamedTempFile, EngineSessionError> {
    let mut bytes = Vec::new();
    for path in files {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
    }
    if bytes.len() as u64 > MAX_POLICY_BYTES {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    let mut list = tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(directory)
        .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
    list.write_all(&bytes)
        .and_then(|()| list.as_file().sync_all())
        .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
    Ok(list)
}

impl Drop for RcloneRuntime {
    fn drop(&mut self) {
        for grant in &self.active_grants {
            self.grants.mark_inactive(*grant);
        }
    }
}

fn map_grant_error(error: AndroidSafGrantError) -> EngineSessionError {
    match error {
        AndroidSafGrantError::Busy => EngineSessionError::InvalidConfiguration,
        AndroidSafGrantError::InvalidGrantId
        | AndroidSafGrantError::InvalidPort
        | AndroidSafGrantError::InvalidCredential
        | AndroidSafGrantError::Unavailable => EngineSessionError::RuntimeUnavailable,
    }
}

fn build_capability_map(
    folders: &BTreeMap<Uuid, RcloneFolder>,
    peers: &BTreeMap<EngineDeviceId, RclonePeer>,
) -> Result<CapabilityMap, EngineSessionError> {
    let mut entries = Vec::new();
    for folder in folders
        .values()
        .filter(|folder| matches!(folder.role, EngineFolderRole::Source))
    {
        let backend = capability_backend(&folder.backend)?;
        for member in &folder.members {
            let Some(peer) = peers.get(member) else {
                continue;
            };
            entries.push(Capability {
                username: folder.id.to_string(),
                public_key: peer.ssh_public_key.clone(),
                backend: backend.clone(),
            });
        }
    }
    if entries.len() > 1024 {
        return Err(EngineSessionError::InvalidConfiguration);
    }
    Ok(CapabilityMap {
        version: 1,
        entries,
    })
}

fn capability_backend(backend: &FolderBackend) -> Result<CapabilityBackend, EngineSessionError> {
    Ok(match backend {
        FolderBackend::Local(path) => CapabilityBackend {
            kind: "local",
            root: path
                .to_str()
                .ok_or(EngineSessionError::InvalidConfiguration)?
                .to_owned(),
            url: None,
            vendor: None,
            user: None,
            pass: None,
        },
        FolderBackend::WebDav {
            address,
            username,
            obscured_password,
            ..
        } => CapabilityBackend {
            kind: "webdav",
            root: String::new(),
            url: Some(format!("http://{address}/")),
            vendor: Some("other"),
            user: Some(username.clone()),
            pass: Some(obscured_password.clone()),
        },
    })
}

fn backend_remote(name: &str, backend: &FolderBackend) -> (OsString, Vec<(OsString, OsString)>) {
    match backend {
        FolderBackend::Local(path) => (path.as_os_str().to_owned(), Vec::new()),
        FolderBackend::WebDav {
            address,
            username,
            obscured_password,
            ..
        } => {
            let prefix = format!("RCLONE_CONFIG_{}", name.to_ascii_uppercase());
            (
                format!("{name}:").into(),
                vec![
                    (format!("{prefix}_TYPE").into(), "webdav".into()),
                    (
                        format!("{prefix}_URL").into(),
                        format!("http://{address}/").into(),
                    ),
                    (format!("{prefix}_VENDOR").into(), "other".into()),
                    (format!("{prefix}_USER").into(), username.into()),
                    (format!("{prefix}_PASS").into(), obscured_password.into()),
                ],
            )
        }
    }
}

fn sftp_environment(
    peer: &RclonePeer,
    folder_id: Uuid,
    key_file: &Path,
    known_hosts: &Path,
) -> Vec<(OsString, OsString)> {
    vec![
        ("RCLONE_CONFIG_SOURCE_TYPE".into(), "sftp".into()),
        (
            "RCLONE_CONFIG_SOURCE_HOST".into(),
            peer.address.ip().to_string().into(),
        ),
        (
            "RCLONE_CONFIG_SOURCE_PORT".into(),
            peer.address.port().to_string().into(),
        ),
        (
            "RCLONE_CONFIG_SOURCE_USER".into(),
            folder_id.to_string().into(),
        ),
        ("RCLONE_CONFIG_SOURCE_KEY_FILE".into(), key_file.into()),
        (
            "RCLONE_CONFIG_SOURCE_KNOWN_HOSTS_FILE".into(),
            known_hosts.into(),
        ),
        (
            "RCLONE_CONFIG_SOURCE_SHA256SUM_COMMAND".into(),
            "sha256sum".into(),
        ),
        ("RCLONE_CONFIG_SOURCE_HASHES".into(), "SHA-256".into()),
    ]
}

fn known_host_name(address: SocketAddr) -> String {
    if address.port() == 22 {
        address.ip().to_string()
    } else {
        format!("[{}]:{}", address.ip(), address.port())
    }
}

fn inventory_index(entries: &[InventoryEntry]) -> EngineIndexSnapshot {
    let bytes = serde_json::to_vec(entries).expect("bounded inventory serializes");
    let digest = Sha256::digest(bytes);
    let mut first = [0_u8; 8];
    first.copy_from_slice(&digest[..8]);
    if first == [0; 8] {
        first[7] = 1;
    }
    EngineIndexSnapshot {
        index_id: format!("0x{:016X}", u64::from_be_bytes(first)),
        sequence: 1,
    }
}

fn prepare_pending_retry(
    state: &mut FolderPolicyState,
    target: &BTreeMap<String, InventoryEntry>,
    restore: bool,
) -> Result<BTreeSet<String>, EngineSessionError> {
    validate_unresolved(state)?;
    if state.pending.is_none() && state.unresolved.is_empty() {
        return Ok(BTreeSet::new());
    }
    // Restoring deleted files grants replay only where the target is absent.
    // An existing unknown original still requires a separate reviewed decision.
    let target_names = restore.then(|| {
        target
            .keys()
            .map(|path| held_path_key(path))
            .collect::<BTreeSet<_>>()
    });
    if let Some(names) = &target_names {
        for attempt in &mut state.unresolved {
            attempt
                .allowed
                .retain(|path| names.contains(&held_path_key(path)));
        }
        state
            .unresolved
            .retain(|attempt| !attempt.allowed.is_empty());
    }
    if let Some(pending) = &state.pending {
        let uncertain = pending
            .allowed
            .iter()
            .filter(|path| {
                !state.owned.contains(*path)
                    && target_names
                        .as_ref()
                        .is_none_or(|names| names.contains(&held_path_key(path)))
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        if !uncertain.is_empty() {
            if state.unresolved.len() >= MAX_UNRESOLVED_TRANSFERS
                || uncertain
                    .iter()
                    .any(|path| !pending.source.contains_key(path))
            {
                return Err(EngineSessionError::RuntimeUnavailable);
            }
            let mut archived = pending.clone();
            archived.allowed = uncertain;
            validate_unresolved_attempt(&archived)?;
            state.unresolved.push(archived);
        }
        state
            .suppressions
            .extend(pending.suppressions.iter().cloned());
        state.pending = None;
        state.copy_completed = false;
    }
    validate_unresolved(state)?;
    Ok(held_paths(state))
}

fn hold_unowned_collisions(
    state: &mut FolderPolicyState,
    source: &BTreeMap<String, InventoryEntry>,
    target: &BTreeMap<String, InventoryEntry>,
    source_index: &EngineIndexSnapshot,
    propagate: bool,
    restore: bool,
) -> Result<(), EngineSessionError> {
    let (allowed, suppressions) = plan_transfer(state, source, target, propagate, restore);
    let target_names = target
        .keys()
        .map(|path| held_path_key(path))
        .collect::<BTreeSet<_>>();
    let collisions = allowed
        .into_iter()
        .filter(|path| {
            source.contains_key(path)
                && !state.owned.contains(path)
                && target_names.contains(&held_path_key(path))
        })
        .collect::<BTreeSet<_>>();
    if !collisions.is_empty() {
        if state.unresolved.len() >= MAX_UNRESOLVED_TRANSFERS {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        state.unresolved.push(PendingTransfer {
            source_index: source_index.clone(),
            source: source.clone(),
            allowed: collisions,
            suppressions,
            propagate_source_deletions: propagate,
        });
        validate_unresolved(state)?;
    }
    Ok(())
}

fn held_paths(state: &FolderPolicyState) -> BTreeSet<String> {
    state
        .unresolved
        .iter()
        .flat_map(|attempt| attempt.allowed.iter())
        .map(|path| held_path_key(path))
        .collect()
}

fn unconfirmed_counts(
    state: &FolderPolicyState,
    active: bool,
) -> Result<(u64, u64), EngineSessionError> {
    validate_unresolved(state)?;
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    let mut count = |pending: &PendingTransfer, only_unowned: bool| {
        if !pending.source_index.is_valid() {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        for path in &pending.allowed {
            if only_unowned && state.owned.contains(path) {
                continue;
            }
            let entry = pending
                .source
                .get(path)
                .filter(|entry| entry.path == *path && entry.size >= 0)
                .ok_or(EngineSessionError::RuntimeUnavailable)?;
            files = files
                .checked_add(1)
                .ok_or(EngineSessionError::RuntimeUnavailable)?;
            bytes = bytes
                .checked_add(entry.size as u64)
                .ok_or(EngineSessionError::RuntimeUnavailable)?;
        }
        Ok::<(), EngineSessionError>(())
    };
    for pending in &state.unresolved {
        count(pending, false)?;
    }
    if let Some(pending) = &state.pending {
        if active
            && pending
                .allowed
                .iter()
                .any(|path| !state.owned.contains(path))
        {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        count(pending, true)?;
    }
    Ok((files, bytes))
}

fn held_path_key(path: &str) -> String {
    // Hold aliases conservatively across case-insensitive/normalizing backends.
    // Stored inventories and delivered paths retain their exact original names.
    path.nfd().default_case_fold().nfd().collect()
}

fn path_is_held(held: &BTreeSet<String>, path: &str) -> bool {
    if held.is_empty() {
        return false;
    }
    let key = held_path_key(path);
    let mut ancestor = key.as_str();
    loop {
        if held.contains(ancestor) {
            return true;
        }
        let Some((parent, _)) = ancestor.rsplit_once('/') else {
            break;
        };
        ancestor = parent;
    }
    let prefix = format!("{key}/");
    held.range::<str, _>((
        std::ops::Bound::Included(prefix.as_str()),
        std::ops::Bound::Unbounded,
    ))
    .next()
    .is_some_and(|candidate| candidate.starts_with(&prefix))
}

fn validate_unresolved_attempt(attempt: &PendingTransfer) -> Result<(), EngineSessionError> {
    if !attempt.source_index.is_valid()
        || attempt.allowed.is_empty()
        || attempt.allowed.iter().any(|path| {
            path.is_empty()
                || path.contains('\0')
                || path.split('/').any(|part| matches!(part, "" | "." | ".."))
                || !Path::new(path)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
                || attempt
                    .source
                    .get(path)
                    .is_none_or(|entry| entry.path != *path || entry.size < 0)
        })
    {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    Ok(())
}

fn validate_unresolved(state: &FolderPolicyState) -> Result<(), EngineSessionError> {
    if state.unresolved.is_empty() {
        return Ok(());
    }
    if state.unresolved.len() > MAX_UNRESOLVED_TRANSFERS {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    let mut paths = BTreeSet::new();
    for attempt in &state.unresolved {
        validate_unresolved_attempt(attempt)?;
        for path in &attempt.allowed {
            if state.owned.contains(path) || !paths.insert(path.clone()) {
                return Err(EngineSessionError::RuntimeUnavailable);
            }
        }
    }
    let held = held_paths(state);
    if state
        .pending
        .as_ref()
        .is_some_and(|pending| pending.allowed.iter().any(|path| path_is_held(&held, path)))
    {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    Ok(())
}

fn plan_transfer(
    state: &FolderPolicyState,
    source: &BTreeMap<String, InventoryEntry>,
    target: &BTreeMap<String, InventoryEntry>,
    propagate: bool,
    restore: bool,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut suppressions = if restore {
        BTreeSet::new()
    } else {
        state.suppressions.clone()
    };
    if !restore {
        for path in &state.owned {
            if source.contains_key(path) && !target.contains_key(path) {
                suppressions.insert(path.clone());
            } else if target.contains_key(path) {
                suppressions.remove(path);
            }
        }
    }
    let mut allowed = source
        .iter()
        .filter(|(path, entry)| state.delivered.get(*path) != Some(*entry))
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    if restore {
        allowed.extend(
            source
                .keys()
                .filter(|path| !target.contains_key(*path))
                .cloned(),
        );
    }
    if propagate {
        allowed.extend(
            state
                .owned
                .iter()
                .filter(|path| !source.contains_key(*path))
                .cloned(),
        );
    }
    let held = held_paths(state);
    allowed.retain(|path| !suppressions.contains(path) && !path_is_held(&held, path));
    (allowed, suppressions)
}

fn complete_transfer_state(
    state: &mut FolderPolicyState,
    source: BTreeMap<String, InventoryEntry>,
    suppressions: BTreeSet<String>,
    propagate: bool,
) {
    let held = held_paths(state);
    let mut verified = source;
    if !held.is_empty() {
        verified.retain(|path, _| !path_is_held(&held, path));
    }
    state.suppressions = suppressions;
    if propagate {
        if held.is_empty() {
            state.delivered = verified;
            state.owned = state.delivered.keys().cloned().collect();
        } else {
            state.delivered.retain(|path, _| path_is_held(&held, path));
            state.owned.retain(|path| path_is_held(&held, path));
            state.delivered.extend(verified);
            state.owned.extend(state.delivered.keys().cloned());
        }
    } else {
        state.owned.extend(verified.keys().cloned());
        state.delivered.extend(verified);
    }
    state.pending = None;
    state.copy_completed = false;
}

fn pending_paths_are_unchanged(
    pending: &PendingTransfer,
    current: &BTreeMap<String, InventoryEntry>,
) -> bool {
    pending
        .allowed
        .iter()
        .all(|path| current.get(path) == pending.source.get(path))
}

#[cfg(test)]
fn verify_transfer(
    pending: &PendingTransfer,
    target: &BTreeMap<String, InventoryEntry>,
) -> Result<(), EngineSessionError> {
    verify_transfer_except(pending, target, &BTreeSet::new())
}

fn verify_transfer_except(
    pending: &PendingTransfer,
    target: &BTreeMap<String, InventoryEntry>,
    held: &BTreeSet<String>,
) -> Result<(), EngineSessionError> {
    for (path, source) in &pending.source {
        if pending.suppressions.contains(path) || path_is_held(held, path) {
            continue;
        }
        if target
            .get(path)
            .is_none_or(|entry| entry.size != source.size)
        {
            return Err(EngineSessionError::EngineUnavailable);
        }
    }
    if pending.propagate_source_deletions
        && pending
            .allowed
            .iter()
            .any(|path| !pending.source.contains_key(path) && target.contains_key(path))
    {
        return Err(EngineSessionError::EngineUnavailable);
    }
    Ok(())
}

fn record_partial_copy(
    path: &Path,
    staged: &Path,
    folder_id: Uuid,
    pending: &PendingTransfer,
    stderr: &[u8],
) -> Result<(), EngineSessionError> {
    let mut copied = BTreeSet::new();
    for line in stderr
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let record: serde_json::Value =
            serde_json::from_slice(line).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let message = record
            .get("msg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let action = message
            .split_once(" to: ")
            .map_or(message, |(action, _)| action);
        if record.get("level").and_then(serde_json::Value::as_str) != Some("info")
            || !matches!(
                action,
                "Copied (new)"
                    | "Copied (replaced existing)"
                    | "Multi-thread Copied (new)"
                    | "Multi-thread Copied (replaced existing)"
                    | "Copied (Rcat, new)"
                    | "Copied (Rcat, replaced existing)"
                    | "Copied (server-side copy)"
            )
        {
            continue;
        }
        let candidate = record
            .get("object")
            .and_then(serde_json::Value::as_str)
            .ok_or(EngineSessionError::RuntimeUnavailable)?;
        let entry = pending
            .source
            .get(candidate)
            .filter(|_| pending.allowed.contains(candidate))
            .ok_or(EngineSessionError::RuntimeUnavailable)?;
        if record.get("size").and_then(serde_json::Value::as_i64) != Some(entry.size) {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        copied.insert(candidate.to_owned());
    }
    if copied.is_empty() {
        return Ok(());
    }
    let mut policy = load_policy(path, staged)?;
    let state = policy.folders.entry(folder_id).or_default();
    if state.pending.as_ref() != Some(pending) {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    // Completed files survive an aggregate failure. Keep the pending operation:
    // missing log records never prove that its other files were left untouched.
    state.owned.extend(copied);
    save_policy(path, staged, &policy)
}

fn record_completed_copy(
    path: &Path,
    staged: &Path,
    folder_id: Uuid,
    pending: &PendingTransfer,
) -> Result<(), EngineSessionError> {
    let mut policy = load_policy(path, staged)?;
    let state = policy.folders.entry(folder_id).or_default();
    if state.pending.as_ref() != Some(pending) {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    state.suppressions = pending.suppressions.clone();
    state.owned.extend(
        pending
            .allowed
            .iter()
            .filter(|candidate| pending.source.contains_key(*candidate))
            .cloned(),
    );
    state.copy_completed = true;
    save_policy(path, staged, &policy)
}

fn clear_known_pending(
    path: &Path,
    staged: &Path,
    folder_id: Uuid,
    pending: &PendingTransfer,
) -> Result<(), EngineSessionError> {
    let mut policy = load_policy(path, staged)?;
    let state = policy.folders.entry(folder_id).or_default();
    if state.pending.as_ref() != Some(pending) {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    state.pending = None;
    state.copy_completed = false;
    save_policy(path, staged, &policy)
}

fn write_private_new(path: &Path, bytes: &[u8], maximum: u64) -> Result<(), EngineSessionError> {
    if bytes.len() as u64 > maximum {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32);
    let mut file = options
        .open(path)
        .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| EngineSessionError::RuntimeUnavailable)
}

fn policy_paths(directory: &Path, folder_id: Uuid) -> (PathBuf, PathBuf) {
    (
        directory.join(format!("rclone-policy-{folder_id}.v1.json")),
        directory.join(format!("rclone-policy-{folder_id}.staged.v1.json")),
    )
}

fn load_policy(path: &Path, staged: &Path) -> Result<PolicyState, EngineSessionError> {
    load_policy_counted(path, staged, MAX_POLICY_BYTES).0
}

fn load_policy_counted(
    path: &Path,
    staged: &Path,
    maximum: u64,
) -> (Result<PolicyState, EngineSessionError>, u64) {
    let mut consumed = 0;
    let result = (|| {
        if staged.exists() {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32,
            )
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PolicyState {
                    version: 1,
                    folders: BTreeMap::new(),
                });
            }
            Err(_) => return Err(EngineSessionError::RuntimeUnavailable),
        };
        let metadata = file
            .metadata()
            .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
            || metadata.len() > maximum
        {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        let mut bytes = Vec::new();
        let read = file.take(maximum + 1).read_to_end(&mut bytes);
        consumed = bytes.len() as u64;
        read.map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        if bytes.len() as u64 > maximum {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        let state: PolicyState =
            serde_json::from_slice(&bytes).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        if state.version != 1 {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        for folder in state.folders.values() {
            validate_unresolved(folder)?;
        }
        Ok(state)
    })();
    (result, consumed)
}

fn save_policy(path: &Path, staged: &Path, state: &PolicyState) -> Result<(), EngineSessionError> {
    for folder in state.folders.values() {
        validate_unresolved(folder)?;
    }
    let bytes = serde_json::to_vec(state).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
    write_private_new(staged, &bytes, MAX_POLICY_BYTES)?;
    fs::rename(staged, path).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
    let parent = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open(
            path.parent()
                .ok_or(EngineSessionError::RuntimeUnavailable)?,
        )
        .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
    parent
        .sync_all()
        .map_err(|_| EngineSessionError::RuntimeUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_runtime_reservations_release_only_their_own_grant() {
        struct Obscurer;
        impl super::super::android_saf::PasswordObscurer for Obscurer {
            fn obscure(
                &self,
                password: zeroize::Zeroizing<String>,
            ) -> Result<zeroize::Zeroizing<String>, AndroidSafGrantError> {
                Ok(password)
            }
        }
        let registry = AndroidSafGrantRegistry::with_obscurer(Arc::new(Obscurer));
        let grant = Uuid::new_v4();
        registry
            .register(
                grant,
                "127.0.0.1:48123".parse().unwrap(),
                zeroize::Zeroizing::new("test-user".to_owned()),
                zeroize::Zeroizing::new("test-password".to_owned()),
            )
            .unwrap();
        let mut original = GrantReservations {
            registry: registry.clone(),
            grants: BTreeSet::new(),
        };
        original.reserve(grant).unwrap();
        {
            let mut preparing = GrantReservations {
                registry: registry.clone(),
                grants: BTreeSet::new(),
            };
            preparing.reserve(grant).unwrap();
            preparing.reserve(grant).unwrap();
            assert_eq!(registry.unregister(grant), Err(AndroidSafGrantError::Busy));
            // A failed preparation drops its reservations while the original runtime remains active.
        }
        assert_eq!(registry.unregister(grant), Err(AndroidSafGrantError::Busy));
        drop(original);
        registry.unregister(grant).unwrap();
    }

    #[test]
    fn transfer_diagnostics_classify_errors_without_disclosing_messages_or_paths() {
        let cases = [
            (
                "HTTP error 403 Forbidden: /private/secret-token",
                "http403,permission",
            ),
            (
                "405 Method Not Allowed: https://user:password@127.0.0.1/secret",
                "http405,unsupported",
            ),
            ("404 Not Found /private/document", "http404,notFound"),
            ("503 Service Unavailable: secret-token", "http503"),
            ("409 Conflict: secret-token", "conflict,http409"),
            ("423 Locked: secret-token", "conflict,http423"),
            ("507 Insufficient Storage: secret-token", "http507,noSpace"),
            (
                "permission denied on read-only filesystem: secret-token",
                "permission,readOnly",
            ),
            ("connection reset by peer: secret-token", "network"),
            ("context deadline exceeded: secret-token", "timeout"),
            ("sizes differ: secret-token", "integrityMismatch"),
            ("sha256 hashes differ: secret-token", "integrityMismatch"),
        ];
        for (message, causes) in cases {
            let stderr = serde_json::to_vec(&serde_json::json!({
                "level": "error", "msg": message, "object": "/private/secret-token"
            }))
            .unwrap();
            assert_eq!(
                transfer_diagnostic_message("sync", "NonzeroExit", Some(3), &stderr),
                format!(
                    "COVALENT_SYNC_DIAGNOSTIC operation=sync category=NonzeroExit exitCode=3 causes={causes}"
                )
            );
        }
        let input = b"ERROR : secret-token: corrupted on transfer
";
        assert_eq!(
            transfer_diagnostic_message("check", "NonzeroExit", Some(1), input),
            "COVALENT_SYNC_DIAGNOSTIC operation=check category=NonzeroExit exitCode=1 causes=integrityMismatch"
        );
        let output = transfer_diagnostic_message(
            "/private/secret-token",
            "password=secret-token",
            None,
            br#"{"level":"info","msg":"Copied (new)","object":"permission denied secret-token"}"#,
        );
        assert_eq!(
            output,
            "COVALENT_SYNC_DIAGNOSTIC operation=other category=Unknown exitCode=none causes=unclassified"
        );
    }

    #[test]
    fn transfer_diagnostics_bound_untrusted_stderr_and_accept_invalid_utf8() {
        let mut stderr = vec![0xff; 100_000];
        stderr.extend_from_slice(
            b"\n{\"level\":\"error\",\"error\":\"status code: 401 secret-token\"}\n",
        );
        let output = transfer_diagnostic_message("lsjson", "NonzeroExit", Some(2), &stderr);
        assert_eq!(
            output,
            "COVALENT_SYNC_DIAGNOSTIC operation=lsjson category=NonzeroExit exitCode=2 causes=http401"
        );
        assert!(output.len() < 160);
    }

    #[test]
    fn temporary_files_list_is_removed_on_early_return() {
        fn fail_after_creation(
            directory: &Path,
            observed_path: &mut Option<PathBuf>,
        ) -> Result<(), EngineSessionError> {
            let list = write_temporary_files_list(
                directory,
                "files-test-transfer-",
                &BTreeSet::from(["one.txt".to_owned(), "two.txt".to_owned()]),
            )?;
            *observed_path = Some(list.path().to_path_buf());
            assert_eq!(fs::read(list.path()).unwrap(), b"one.txt\0two.txt\0");
            Err(EngineSessionError::TransferFailed)
        }

        let directory = tempfile::tempdir().unwrap();
        let mut observed_path = None;
        assert_eq!(
            fail_after_creation(directory.path(), &mut observed_path),
            Err(EngineSessionError::TransferFailed)
        );
        let observed_path = observed_path.unwrap();
        assert_eq!(observed_path.parent(), Some(directory.path()));
        assert!(!observed_path.exists());
    }

    #[test]
    fn inventory_index_is_stable_and_nonzero() {
        let rows = vec![InventoryEntry {
            path: "line\nname/}}.txt".to_owned(),
            size: 3,
            modified_unix_seconds: 1,
            sha256: Some("00".repeat(32)),
        }];
        let one = inventory_index(&rows);
        assert!(one.is_valid());
        assert_eq!(one, inventory_index(&rows));
    }

    #[test]
    fn transfer_verification_preserves_suppressed_and_unrelated_paths() {
        let source = InventoryEntry {
            path: "source.txt".into(),
            size: 2,
            modified_unix_seconds: 1,
            sha256: Some("00".repeat(32)),
        };
        let pending = PendingTransfer {
            source_index: inventory_index(std::slice::from_ref(&source)),
            source: BTreeMap::from([(source.path.clone(), source.clone())]),
            allowed: BTreeSet::from([source.path.clone(), "old.txt".into()]),
            suppressions: BTreeSet::new(),
            propagate_source_deletions: true,
        };
        assert!(
            verify_transfer(&pending, &BTreeMap::from([(source.path.clone(), source)])).is_ok()
        );
    }

    #[test]
    fn empty_run_persists_target_deletion_suppression_without_transfer() {
        let directory = tempfile::tempdir().unwrap();
        let folder = Uuid::new_v4();
        let (path, staged) = policy_paths(directory.path(), folder);
        let source = InventoryEntry {
            path: "deleted-at-target.txt".into(),
            size: 4,
            modified_unix_seconds: 1,
            sha256: Some("11".repeat(32)),
        };
        let source_entries = BTreeMap::from([(source.path.clone(), source.clone())]);
        let mut policy = PolicyState {
            version: 1,
            folders: BTreeMap::from([(
                folder,
                FolderPolicyState {
                    owned: BTreeSet::from([source.path.clone()]),
                    delivered: source_entries.clone(),
                    ..FolderPolicyState::default()
                },
            )]),
        };

        let state = policy.folders.get_mut(&folder).unwrap();
        let (allowed, suppressions) =
            plan_transfer(state, &source_entries, &BTreeMap::new(), false, false);
        assert!(allowed.is_empty());
        assert_eq!(suppressions, BTreeSet::from([source.path.clone()]));
        let pending = PendingTransfer {
            source_index: inventory_index(std::slice::from_ref(&source)),
            source: source_entries.clone(),
            allowed,
            suppressions,
            propagate_source_deletions: false,
        };
        assert!(verify_transfer(&pending, &BTreeMap::new()).is_ok());

        complete_transfer_state(state, source_entries, pending.suppressions, false);
        save_policy(&path, &staged, &policy).unwrap();

        let persisted = load_policy(&path, &staged).unwrap();
        let state = &persisted.folders[&folder];
        assert_eq!(state.suppressions, BTreeSet::from([source.path.clone()]));
        assert_eq!(state.owned, BTreeSet::from([source.path.clone()]));
        assert_eq!(state.delivered.get(&source.path), Some(&source));
        assert!(state.pending.is_none());
        assert!(!state.copy_completed);
    }

    #[test]
    fn empty_run_rejects_existing_target_with_wrong_size() {
        let source = InventoryEntry {
            path: "wrong-size.txt".into(),
            size: 4,
            modified_unix_seconds: 1,
            sha256: Some("11".repeat(32)),
        };
        let source_entries = BTreeMap::from([(source.path.clone(), source.clone())]);
        let state = FolderPolicyState {
            owned: BTreeSet::from([source.path.clone()]),
            delivered: source_entries.clone(),
            ..FolderPolicyState::default()
        };
        let target = InventoryEntry {
            size: 5,
            sha256: None,
            ..source.clone()
        };
        let target_entries = BTreeMap::from([(target.path.clone(), target)]);
        let (allowed, suppressions) =
            plan_transfer(&state, &source_entries, &target_entries, false, false);
        assert!(allowed.is_empty());
        assert!(suppressions.is_empty());
        let pending = PendingTransfer {
            source_index: inventory_index(std::slice::from_ref(&source)),
            source: source_entries,
            allowed,
            suppressions,
            propagate_source_deletions: false,
        };

        assert!(matches!(
            verify_transfer(&pending, &target_entries),
            Err(EngineSessionError::EngineUnavailable)
        ));
    }

    #[test]
    fn known_stopped_attempt_retains_copied_ownership_and_clears_pending() {
        let directory = tempfile::tempdir().unwrap();
        let folder = Uuid::new_v4();
        let (path, staged) = policy_paths(directory.path(), folder);
        let source = InventoryEntry {
            path: "copied.txt".into(),
            size: 4,
            modified_unix_seconds: 1,
            sha256: Some("11".repeat(32)),
        };
        let pending = PendingTransfer {
            source_index: inventory_index(std::slice::from_ref(&source)),
            source: BTreeMap::from([(source.path.clone(), source.clone())]),
            allowed: BTreeSet::from([source.path.clone()]),
            suppressions: BTreeSet::new(),
            propagate_source_deletions: false,
        };
        let mut policy = PolicyState {
            version: 1,
            folders: BTreeMap::new(),
        };
        policy.folders.insert(
            folder,
            FolderPolicyState {
                pending: Some(pending.clone()),
                ..FolderPolicyState::default()
            },
        );
        save_policy(&path, &staged, &policy).unwrap();

        record_completed_copy(&path, &staged, folder, &pending).unwrap();
        let interrupted = load_policy(&path, &staged).unwrap();
        assert!(interrupted.folders[&folder].copy_completed);
        assert_eq!(
            interrupted.folders[&folder].owned,
            BTreeSet::from([source.path.clone()])
        );
        clear_known_pending(&path, &staged, folder, &pending).unwrap();

        let state = load_policy(&path, &staged).unwrap();
        let folder_state = &state.folders[&folder];
        assert_eq!(folder_state.owned, BTreeSet::from([source.path]));
        assert!(folder_state.delivered.is_empty());
        assert!(folder_state.pending.is_none());
        assert!(!folder_state.copy_completed);
    }

    #[test]
    fn partial_copy_preserves_confirmed_ownership_without_clearing_uncertainty() {
        let directory = tempfile::tempdir().unwrap();
        let folder = Uuid::new_v4();
        let (path, staged) = policy_paths(directory.path(), folder);
        let source = ["copied.txt", "replaced.txt", "unfinished.txt"]
            .map(|name| {
                let entry = InventoryEntry {
                    path: name.into(),
                    size: 4,
                    modified_unix_seconds: 1,
                    sha256: Some("11".repeat(32)),
                };
                (entry.path.clone(), entry)
            })
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let pending = PendingTransfer {
            source_index: inventory_index(&source.values().cloned().collect::<Vec<_>>()),
            source: source.clone(),
            allowed: source.keys().cloned().collect(),
            suppressions: BTreeSet::new(),
            propagate_source_deletions: true,
        };
        let policy = PolicyState {
            version: 1,
            folders: BTreeMap::from([(
                folder,
                FolderPolicyState {
                    pending: Some(pending.clone()),
                    ..FolderPolicyState::default()
                },
            )]),
        };
        save_policy(&path, &staged, &policy).unwrap();
        let log = b"{\"level\":\"info\",\"msg\":\"Copied (new)\",\"object\":\"copied.txt\",\"size\":4}\n\
            {\"level\":\"info\",\"msg\":\"Multi-thread Copied (replaced existing) to: replaced.txt\",\"object\":\"replaced.txt\",\"size\":4}\n\
            {\"level\":\"error\",\"msg\":\"Failed to copy\",\"object\":\"unfinished.txt\"}\n";
        record_partial_copy(&path, &staged, folder, &pending, log).unwrap();
        let recovered = load_policy(&path, &staged).unwrap();
        let state = &recovered.folders[&folder];
        assert_eq!(
            state.owned,
            BTreeSet::from(["copied.txt".into(), "replaced.txt".into()])
        );
        assert!(state.delivered.is_empty());
        assert_eq!(state.pending.as_ref(), Some(&pending));
        assert!(!state.copy_completed);

        let mut target = source.clone();
        target.insert("unrelated.txt".into(), source["copied.txt"].clone());
        let mut after_source_deletion = source.clone();
        after_source_deletion.remove("copied.txt");
        let mut restoring = state.clone();
        assert_eq!(
            prepare_pending_retry(&mut restoring, &target, true).unwrap(),
            BTreeSet::from(["unfinished.txt".into()])
        );
        let (allowed, _) = plan_transfer(&restoring, &after_source_deletion, &target, true, true);
        assert!(allowed.contains("copied.txt"));
        assert!(!allowed.contains("unrelated.txt"));

        target.remove("copied.txt");
        let (allowed, suppressions) = plan_transfer(state, &source, &target, false, false);
        assert!(!allowed.contains("copied.txt"));
        assert!(suppressions.contains("copied.txt"));
        let mut conservative = state.clone();
        assert_eq!(
            prepare_pending_retry(&mut conservative, &target, false).unwrap(),
            BTreeSet::from(["unfinished.txt".into()])
        );
        assert_eq!(conservative.unresolved[0].source, pending.source);
        assert_eq!(
            conservative.unresolved[0].allowed,
            BTreeSet::from(["unfinished.txt".into()])
        );
        assert!(!conservative.owned.contains("unfinished.txt"));
        assert!(conservative.pending.is_none());

        // A WebDAV write can appear before an error without a completed-copy
        // record. Restoration must not discard that unknown file's ownership.
        let mut missing_unconfirmed_source = source.clone();
        missing_unconfirmed_source.remove("unfinished.txt");
        let mut preserve_unknown = state.clone();
        assert_eq!(
            prepare_pending_retry(&mut preserve_unknown, &target, true).unwrap(),
            BTreeSet::from(["unfinished.txt".into()])
        );
        let (allowed, _) = plan_transfer(
            &preserve_unknown,
            &missing_unconfirmed_source,
            &target,
            true,
            true,
        );
        assert!(
            !allowed.contains("unfinished.txt"),
            "restoration never grants deletion ownership of an unknown file"
        );
        target.remove("unfinished.txt");
        let mut absent_unknown = state.clone();
        assert!(
            prepare_pending_retry(&mut absent_unknown, &target, true)
                .unwrap()
                .is_empty()
        );

        let before_invalid = fs::read(&path).unwrap();
        for invalid in [
            b"{\"level\":\"info\",\"msg\":\"Copied (new)\",\"object\":\"unrelated.txt\",\"size\":4}".as_slice(),
            b"{\"level\":\"info\",\"msg\":\"Copied (new)\",\"object\":\"unfinished.txt\",\"size\":5}",
            b"{\"level\":\"info\",\"msg\":\"Copied (new)\",\"object\":\"unfinished.txt\",\"size\":4}\n{",
        ] {
            assert_eq!(
                record_partial_copy(&path, &staged, folder, &pending, invalid),
                Err(EngineSessionError::RuntimeUnavailable)
            );
            assert_eq!(fs::read(&path).unwrap(), before_invalid);
        }
    }

    fn uncertain_fixture(name: &str) -> PendingTransfer {
        let entry = InventoryEntry {
            path: name.into(),
            size: 4,
            modified_unix_seconds: 1,
            sha256: Some("11".repeat(32)),
        };
        PendingTransfer {
            source_index: inventory_index(std::slice::from_ref(&entry)),
            source: BTreeMap::from([(name.into(), entry)]),
            allowed: BTreeSet::from([name.into()]),
            suppressions: BTreeSet::new(),
            propagate_source_deletions: true,
        }
    }

    #[test]
    fn held_name_aliases_include_ancestors_without_hiding_siblings() {
        let held = ["a-.txt", "a/b.jpg", "Café/IMG.JPG", "Straße/file.txt"]
            .into_iter()
            .map(held_path_key)
            .collect();
        for path in [
            "a",
            "a/b.jpg",
            "a/b.jpg/child",
            "CAFÉ/img.jpg",
            "Cafe\u{301}/img.jpg",
            "STRASSE/file.txt",
            "strasse",
        ] {
            assert!(path_is_held(&held, path), "alias escaped: {path}");
        }
        for path in [
            "a-new.txt",
            "a-more/file.txt",
            "a/c.jpg",
            "Café/other.jpg",
            "Straße2/file.txt",
        ] {
            assert!(!path_is_held(&held, path), "unrelated sibling held: {path}");
        }
    }

    #[test]
    fn unresolved_writes_survive_new_success_without_ownership_or_deletion() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegacyFolder {
            #[serde(rename = "owned")]
            _owned: Option<BTreeSet<String>>,
            #[serde(rename = "delivered")]
            _delivered: Option<BTreeMap<String, InventoryEntry>>,
            #[serde(rename = "suppressions")]
            _suppressions: Option<BTreeSet<String>>,
            #[serde(rename = "pending")]
            _pending: Option<PendingTransfer>,
            #[serde(rename = "copyCompleted")]
            _copy_completed: Option<bool>,
        }
        // Use exact legacy field names while refusing the new durable field.
        let mut state = FolderPolicyState {
            pending: Some(uncertain_fixture("held.txt")),
            ..Default::default()
        };
        let original = state.pending.clone().unwrap();
        let unknown = InventoryEntry {
            size: 99,
            ..original.source["held.txt"].clone()
        };
        let target = BTreeMap::from([("held.txt".into(), unknown)]);
        assert_eq!(
            prepare_pending_retry(&mut state, &target, false).unwrap(),
            BTreeSet::from(["held.txt".into()])
        );
        let fresh = uncertain_fixture("fresh.txt").source;
        let index = inventory_index(&fresh.values().cloned().collect::<Vec<_>>());
        hold_unowned_collisions(&mut state, &fresh, &target, &index, true, false).unwrap();
        let (allowed, suppressions) = plan_transfer(&state, &fresh, &target, true, false);
        assert_eq!(allowed, BTreeSet::from(["fresh.txt".into()]));
        let attempt = PendingTransfer {
            source_index: index,
            source: fresh.clone(),
            allowed,
            suppressions: suppressions.clone(),
            propagate_source_deletions: true,
        };
        let mut after = target.clone();
        after.extend(fresh.clone());
        verify_transfer_except(&attempt, &after, &held_paths(&state)).unwrap();
        complete_transfer_state(&mut state, fresh, suppressions, true);
        assert_eq!(state.unresolved, vec![original]);
        assert!(state.pending.is_none());
        assert_eq!(state.owned, BTreeSet::from(["fresh.txt".into()]));
        assert!(!state.delivered.contains_key("held.txt"));
        let directory = tempfile::tempdir().unwrap();
        let folder = Uuid::new_v4();
        let (path, staged) = policy_paths(directory.path(), folder);
        save_policy(
            &path,
            &staged,
            &PolicyState {
                version: 1,
                folders: BTreeMap::from([(folder, state.clone())]),
            },
        )
        .unwrap();
        let reopened = load_policy(&path, &staged).unwrap();
        assert_eq!(reopened.folders[&folder].unresolved, state.unresolved);
        assert!(
            serde_json::from_value::<LegacyFolder>(
                serde_json::to_value(FolderPolicyState::default()).unwrap()
            )
            .is_ok()
        );
        let encoded = serde_json::to_value(&state).unwrap();
        assert!(serde_json::from_value::<LegacyFolder>(encoded).is_err());
        // Restoration never overwrites an existing unknown original.
        assert!(
            !prepare_pending_retry(&mut state, &after, true)
                .unwrap()
                .is_empty()
        );
        after.remove("held.txt");
        assert!(
            prepare_pending_retry(&mut state, &after, false)
                .unwrap()
                .contains("held.txt")
        );
        assert!(
            prepare_pending_retry(&mut state, &after, true)
                .unwrap()
                .is_empty()
        );
        assert!(!state.owned.contains("held.txt"));
    }

    #[test]
    fn unowned_target_collisions_hold_aliases_and_keep_new_paths_available() {
        let pending = uncertain_fixture("Café/IMG.JPG");
        let source = pending.source;
        let existing = InventoryEntry {
            path: "Cafe\u{301}/img.jpg".into(),
            size: 99,
            modified_unix_seconds: 2,
            sha256: None,
        };
        let target = BTreeMap::from([(existing.path.clone(), existing)]);
        let mut state = FolderPolicyState::default();
        hold_unowned_collisions(
            &mut state,
            &source,
            &target,
            &pending.source_index,
            true,
            true,
        )
        .unwrap();
        assert_eq!(state.unresolved.len(), 1);
        assert!(!state.owned.contains("Café/IMG.JPG"));
        assert!(
            !prepare_pending_retry(&mut state, &target, true)
                .unwrap()
                .is_empty()
        );
        let mut next = source;
        next.extend(uncertain_fixture("new.jpg").source);
        let (allowed, _) = plan_transfer(&state, &next, &target, true, false);
        assert_eq!(allowed, BTreeSet::from(["new.jpg".into()]));
    }

    #[test]
    fn malformed_or_excessive_unresolved_state_never_replaces_policy() {
        let directory = tempfile::tempdir().unwrap();
        let folder = Uuid::new_v4();
        let (path, staged) = policy_paths(directory.path(), folder);
        let mut state = FolderPolicyState {
            unresolved: vec![uncertain_fixture("held.txt")],
            ..Default::default()
        };
        let mut policy = PolicyState {
            version: 1,
            folders: BTreeMap::from([(folder, state.clone())]),
        };
        save_policy(&path, &staged, &policy).unwrap();
        let before = fs::read(&path).unwrap();
        for name in ["../escape", "/absolute", "a//b", "a/./b", "a/../b"] {
            state.unresolved = vec![uncertain_fixture(name)];
            policy.folders.insert(folder, state.clone());
            assert_eq!(
                save_policy(&path, &staged, &policy),
                Err(EngineSessionError::RuntimeUnavailable)
            );
            assert_eq!(fs::read(&path).unwrap(), before);
            assert!(!staged.exists());
        }
        state.unresolved = vec![uncertain_fixture("same.txt"), uncertain_fixture("same.txt")];
        assert_eq!(
            validate_unresolved(&state),
            Err(EngineSessionError::RuntimeUnavailable)
        );
        state.unresolved = (0..MAX_UNRESOLVED_TRANSFERS)
            .map(|n| uncertain_fixture(&format!("held-{n}.txt")))
            .collect();
        assert!(validate_unresolved(&state).is_ok());
        state.pending = Some(uncertain_fixture("one-too-many.txt"));
        assert_eq!(
            prepare_pending_retry(&mut state, &BTreeMap::new(), false),
            Err(EngineSessionError::RuntimeUnavailable)
        );
        state.unresolved.push(uncertain_fixture("one-too-many.txt"));
        policy.folders.insert(folder, state);
        assert_eq!(
            save_policy(&path, &staged, &policy),
            Err(EngineSessionError::RuntimeUnavailable)
        );
        // Persisted files remain the original valid snapshot after failed admission.
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn recovery_counts_are_unknown_during_unknown_writes_and_read_budget_is_total() {
        let pending = uncertain_fixture("unknown.txt");
        let state = FolderPolicyState {
            pending: Some(pending),
            ..Default::default()
        };
        assert_eq!(unconfirmed_counts(&state, false), Ok((1, 4)));
        assert_eq!(
            unconfirmed_counts(&state, true),
            Err(EngineSessionError::RuntimeUnavailable)
        );
        let root = tempfile::tempdir().unwrap();
        let sync = root.path().join("sync");
        fs::create_dir(&sync).unwrap();
        fs::set_permissions(&sync, fs::Permissions::from_mode(0o700)).unwrap();
        let protector = covalent_core::StaticKeyProtector::new(1, [42; 32]).unwrap();
        let installation = Arc::new(EngineInstallation::create(&sync, &protector).unwrap());
        let directory = installation.database_directory().unwrap();
        let mut large = uncertain_fixture("unknown.txt");
        for n in 0..6000 {
            let name = format!(
                "folder-{n}/{}/{}/file.jpg",
                "a".repeat(200),
                "b".repeat(200)
            );
            large.source.insert(
                name.clone(),
                InventoryEntry {
                    path: name,
                    size: 1,
                    modified_unix_seconds: 1,
                    sha256: Some("11".repeat(32)),
                },
            );
        }
        // A full admitted snapshot can be larger than its small held path set.
        large.source_index = inventory_index(&large.source.values().cloned().collect::<Vec<_>>());
        let folders = BTreeSet::from([Uuid::new_v4(), Uuid::new_v4()]);
        for folder in &folders {
            let policy = PolicyState {
                version: 1,
                folders: BTreeMap::from([(
                    *folder,
                    FolderPolicyState {
                        unresolved: vec![large.clone()],
                        ..Default::default()
                    },
                )]),
            };
            let (path, staged) = policy_paths(&directory, *folder);
            save_policy(&path, &staged, &policy).unwrap();
            assert!(fs::metadata(&path).unwrap().len() > MAX_RECOVERY_READ_BYTES / 2);
            assert!(fs::metadata(&path).unwrap().len() < MAX_RECOVERY_READ_BYTES);
        }
        let summaries = RecoveryReader::new(installation, folders, BTreeSet::new()).read();
        assert_eq!(summaries.iter().filter(|row| row.available).count(), 1);
        assert_eq!(
            summaries
                .iter()
                .filter(|row| !row.available
                    && row.unconfirmed_files.is_none()
                    && row.unconfirmed_source_bytes.is_none())
                .count(),
            1
        );
    }
}
