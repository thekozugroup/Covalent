//! Exact rclone process inputs and durable one-way deletion policy state.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::android_saf::{AndroidSafGrantError, AndroidSafGrantRegistry};
use super::config::{EngineDeviceId, EngineFolderConfig, EngineFolderRole, EnginePeerConfig};
use super::supervisor::OwnedRcloneCommand;
use super::{EngineIndexSnapshot, EngineSessionError, VerifiedEngineExecutable};

const MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_POLICY_BYTES: u64 = 64 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);

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

pub(super) struct RcloneRuntime {
    guardian: Arc<VerifiedEngineExecutable>,
    executable: Arc<VerifiedEngineExecutable>,
    runtime: Arc<tempfile::TempDir>,
    key_file: PathBuf,
    known_hosts: BTreeMap<EngineDeviceId, PathBuf>,
    folders: BTreeMap<Uuid, RcloneFolder>,
    peers: BTreeMap<EngineDeviceId, RclonePeer>,
    policy_directory: PathBuf,
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
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PolicyState {
    version: u8,
    folders: BTreeMap<Uuid, FolderPolicyState>,
}

enum RcloneCommandFailure {
    Uncertain(EngineSessionError),
    ReapedNonzero,
}

impl RcloneCommandFailure {
    fn into_session_error(self) -> EngineSessionError {
        match self {
            Self::Uncertain(error) => error,
            Self::ReapedNonzero => EngineSessionError::EngineUnavailable,
        }
    }
}

impl RcloneRuntime {
    pub(super) fn executable(&self) -> &VerifiedEngineExecutable {
        &self.executable
    }

    pub(super) async fn prepare(
        guardian: Arc<VerifiedEngineExecutable>,
        executable: Arc<VerifiedEngineExecutable>,
        runtime_parent: &Path,
        policy_directory: &Path,
        identity_key: &str,
        folders: &[EngineFolderConfig],
        peers: &[EnginePeerConfig],
    ) -> Result<Self, EngineSessionError> {
        executable
            .recheck()
            .map_err(|_| EngineSessionError::LaunchFailed)?;
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
        write_private_new(&key_file, identity_key.as_bytes(), 4096)?;
        let grants = executable.android_saf_grants();
        let mut active_grants = BTreeSet::new();
        let mut resolved_folders = BTreeMap::new();
        for folder in folders {
            if matches!(folder.role(), EngineFolderRole::LegacyTwoWay) {
                return Err(EngineSessionError::InvalidConfiguration);
            }
            let backend = if let Some((grant_id, grant)) =
                grants.resolve(folder.root()).map_err(map_grant_error)?
            {
                grants.mark_active(grant_id).map_err(map_grant_error)?;
                active_grants.insert(grant_id);
                FolderBackend::WebDav {
                    grant_id,
                    address: grant.address,
                    username: grant.username.to_string(),
                    obscured_password: grant.password.to_string(),
                }
            } else {
                FolderBackend::Local(folder.root().to_path_buf())
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
        for peer in peers {
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
        for folder_id in resolved_folders.keys() {
            let (policy, staged) = policy_paths(policy_directory, *folder_id);
            load_policy(&policy, &staged)?;
        }
        Ok(Self {
            guardian,
            executable,
            runtime: Arc::new(runtime),
            key_file,
            known_hosts,
            folders: resolved_folders,
            peers: resolved_peers,
            policy_directory: policy_directory.to_path_buf(),
            grants,
            active_grants,
        })
    }

    pub(super) fn server_inputs(
        &self,
        listener: SocketAddr,
    ) -> Result<(Vec<OsString>, Vec<(OsString, OsString)>), EngineSessionError> {
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
            .map(|entry| entry.path)
            .collect::<BTreeSet<_>>();
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
        if state.pending.is_some() {
            // The transfer may have completed before the process stopped. Replaying
            // could restore a destination deletion that happened after that point.
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        let mut suppressions = if restore {
            BTreeSet::new()
        } else {
            state.suppressions.clone()
        };
        if !restore {
            for path in &state.owned {
                if source.contains_key(path) && !target.contains(path) {
                    suppressions.insert(path.clone());
                } else if target.contains(path) {
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
                    .filter(|path| !target.contains(*path))
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
        allowed.retain(|path| !suppressions.contains(path));
        let pending = PendingTransfer {
            source_index: source_index.clone(),
            source: source.clone(),
            allowed,
            suppressions,
            propagate_source_deletions: propagate,
        };
        state.pending = Some(pending.clone());
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
            self.run_transfer(folder, peer, &list, pending.propagate_source_deletions)
                .await?;
            record_completed_copy(&policy_path, &policy_staged, folder_id, &pending)?;
            let after_copy = self
                .list_sftp(folder.id, peer)
                .await?
                .into_iter()
                .map(|entry| (entry.path.clone(), entry))
                .collect::<BTreeMap<_, _>>();
            if !pending_paths_are_unchanged(&pending, &after_copy) {
                return Err(EngineSessionError::TransferFailed);
            }
            if verified.is_empty() {
                Ok(())
            } else {
                self.check_transfer(folder, peer, &verification_list).await
            }
        }
        .await;
        let _ = fs::remove_file(&list);
        let _ = fs::remove_file(&verification_list);
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
        verify_transfer(&pending, &target_after)?;
        let mut next = load_policy(&policy_path, &policy_staged)?;
        let state = next.folders.entry(folder_id).or_default();
        if state
            .pending
            .as_ref()
            .is_none_or(|current| current.source_index != source_index)
        {
            return Err(EngineSessionError::EngineUnavailable);
        }
        state.suppressions = pending.suppressions;
        if propagate {
            state.delivered = source;
            state.owned = state.delivered.keys().cloned().collect();
        } else {
            state.owned.extend(source.keys().cloned());
            state.delivered.extend(source);
        }
        state.pending = None;
        save_policy(&policy_path, &policy_staged, &next)?;
        Ok(source_index)
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
        propagate: bool,
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
            OsString::from(if propagate { "sync" } else { "copy" }),
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
        ];
        self.run(&args, &environment)
            .await
            .map(|_| ())
            .map_err(RcloneCommandFailure::into_session_error)
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
                RcloneCommandFailure::ReapedNonzero => EngineSessionError::TransferFailed,
                RcloneCommandFailure::Uncertain(error) => error,
            })
    }

    fn write_files_list(
        &self,
        folder_id: Uuid,
        files: &BTreeSet<String>,
    ) -> Result<PathBuf, EngineSessionError> {
        self.write_files_list_with_suffix(folder_id, "transfer", files)
    }

    fn write_files_list_with_suffix(
        &self,
        folder_id: Uuid,
        suffix: &str,
        files: &BTreeSet<String>,
    ) -> Result<PathBuf, EngineSessionError> {
        let path = self
            .runtime
            .path()
            .join(format!("files-{folder_id}-{suffix}"));
        let mut bytes = Vec::new();
        for path in files {
            bytes.extend_from_slice(path.as_bytes());
            bytes.push(0);
        }
        write_private_new(&path, &bytes, MAX_POLICY_BYTES)?;
        Ok(path)
    }

    async fn run(
        &self,
        args: &[OsString],
        environment: &[(OsString, OsString)],
    ) -> Result<Vec<u8>, RcloneCommandFailure> {
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
        let command = OwnedRcloneCommand::launch(
            &self.guardian,
            &self.executable,
            args,
            &owned_environment,
            self.runtime.path(),
            MAX_OUTPUT_BYTES,
            Box::new(Arc::clone(&self.runtime)),
        )
        .map_err(|_| RcloneCommandFailure::Uncertain(EngineSessionError::LaunchFailed))?;
        let output = tokio::time::timeout(COMMAND_TIMEOUT, command.wait())
            .await
            .map_err(|_| RcloneCommandFailure::Uncertain(EngineSessionError::EngineUnavailable))?
            .map_err(|_| RcloneCommandFailure::Uncertain(EngineSessionError::EngineUnavailable))?;
        if !output.status.success() {
            return Err(RcloneCommandFailure::ReapedNonzero);
        }
        let _discarded_stderr = output.stderr;
        Ok(output.stdout)
    }
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

fn pending_paths_are_unchanged(
    pending: &PendingTransfer,
    current: &BTreeMap<String, InventoryEntry>,
) -> bool {
    pending
        .allowed
        .iter()
        .all(|path| current.get(path) == pending.source.get(path))
}

fn verify_transfer(
    pending: &PendingTransfer,
    target: &BTreeMap<String, InventoryEntry>,
) -> Result<(), EngineSessionError> {
    for (path, source) in &pending.source {
        if pending.suppressions.contains(path) {
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
    if staged.exists() {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
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
        || metadata.len() > MAX_POLICY_BYTES
    {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    let mut bytes = Vec::new();
    file.take(MAX_POLICY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
    if bytes.len() as u64 > MAX_POLICY_BYTES {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    let state: PolicyState =
        serde_json::from_slice(&bytes).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
    if state.version != 1 {
        return Err(EngineSessionError::RuntimeUnavailable);
    }
    Ok(state)
}

fn save_policy(path: &Path, staged: &Path, state: &PolicyState) -> Result<(), EngineSessionError> {
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
        clear_known_pending(&path, &staged, folder, &pending).unwrap();

        let state = load_policy(&path, &staged).unwrap();
        let folder_state = &state.folders[&folder];
        assert_eq!(folder_state.owned, BTreeSet::from([source.path]));
        assert!(folder_state.delivered.is_empty());
        assert!(folder_state.pending.is_none());
    }
}
