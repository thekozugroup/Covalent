//! One bounded rclone session beneath a caller-owned durable installation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::net::SocketAddr;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::config::{EngineFolderConfig, EngineFolderRole, EnginePeerConfig};
use super::installation::{EngineInstallation, EngineWorkerLease};
use super::rclone::RcloneRuntime;
use super::{
    AndroidSafGrantRegistry, EngineIndexSnapshot, EnginePeerConnection, EnginePeerConnectionState,
    FolderHealth, FolderLifecycle, OwnedEngineWorker, StopOutcome, VerifiedEngineExecutable,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Already-authorized desired state for one engine session.
#[derive(Clone, Eq, PartialEq)]
pub struct EngineSessionSettings {
    pub device_name: String,
    pub listener: Option<SocketAddr>,
    pub peers: Vec<EnginePeerConfig>,
    pub folders: Vec<EngineFolderConfig>,
}

/// Redacted session failure. Details remain inside the local engine boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineSessionError {
    InvalidConfiguration,
    RuntimeUnavailable,
    LaunchFailed,
    StartupTimeout,
    StartupMismatch,
    EngineUnavailable,
    TransferFailed,
}

impl fmt::Display for EngineSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "folder sync configuration is invalid",
            Self::RuntimeUnavailable => "folder sync runtime storage is unavailable",
            Self::LaunchFailed => "folder sync worker could not start",
            Self::StartupTimeout => "folder sync worker did not become ready",
            Self::StartupMismatch => "folder sync worker failed startup verification",
            Self::EngineUnavailable => "folder sync worker is unavailable",
            Self::TransferFailed => "folder transfer did not complete",
        })
    }
}

impl std::error::Error for EngineSessionError {}

pub struct ManagedEngineSession {
    worker: Option<OwnedEngineWorker>,
    guardian: Arc<VerifiedEngineExecutable>,
    runtime: Arc<RcloneRuntime>,
    installation: Arc<EngineInstallation>,
    worker_lease: Option<Arc<EngineWorkerLease>>,
    roots: Arc<Vec<FolderRootLease>>,
    settings: EngineSessionSettings,
    unavailable_folders: BTreeSet<Uuid>,
    jobs: BTreeMap<Uuid, JoinHandle<Result<EngineIndexSnapshot, EngineSessionError>>>,
    completed_peers: BTreeSet<super::config::EngineDeviceId>,
    closed: bool,
}

pub(super) struct InitialScanTask {
    task: Option<JoinHandle<Result<(), EngineSessionError>>>,
}

impl InitialScanTask {
    pub(super) fn new(task: JoinHandle<Result<(), EngineSessionError>>) -> Self {
        Self { task: Some(task) }
    }

    pub(super) fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub(super) fn abort(&self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }

    pub(super) async fn finish(&mut self) -> Result<(), EngineSessionError> {
        self.task
            .take()
            .ok_or(EngineSessionError::EngineUnavailable)?
            .await
            .map_err(|_| EngineSessionError::EngineUnavailable)?
    }
}

impl Drop for InitialScanTask {
    fn drop(&mut self) {
        self.abort();
    }
}

impl fmt::Debug for ManagedEngineSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagedEngineSession([PRIVATE])")
    }
}

pub(super) struct FolderRootLease {
    path: PathBuf,
    file: File,
    identity: (u64, u64),
}

struct ServerKeepalive {
    _runtime: Arc<RcloneRuntime>,
    _installation: Arc<EngineInstallation>,
    _roots: Arc<Vec<FolderRootLease>>,
    _worker_lease: Arc<EngineWorkerLease>,
}

impl ManagedEngineSession {
    pub async fn start(
        installation: Arc<EngineInstallation>,
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        runtime_parent: &Path,
        settings: EngineSessionSettings,
    ) -> Result<Self, EngineSessionError> {
        Self::start_with_gate(installation, guardian, engine, runtime_parent, settings).await
    }

    pub(super) async fn start_reset_gate(
        installation: Arc<EngineInstallation>,
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        runtime_parent: &Path,
        settings: EngineSessionSettings,
    ) -> Result<Self, EngineSessionError> {
        Self::start_with_gate(installation, guardian, engine, runtime_parent, settings).await
    }

    async fn start_with_gate(
        installation: Arc<EngineInstallation>,
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        runtime_parent: &Path,
        settings: EngineSessionSettings,
    ) -> Result<Self, EngineSessionError> {
        installation
            .revalidate()
            .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        let (runtime_settings, unavailable_folders) =
            available_runtime_settings(&settings, &engine.android_saf_grants())?;
        let worker_lease = Arc::new(
            installation
                .claim_worker()
                .map_err(|_| EngineSessionError::LaunchFailed)?,
        );
        let roots = Arc::new(
            runtime_settings
                .folders
                .iter()
                .filter(|folder| {
                    super::android_saf::parse_token(folder.root())
                        .ok()
                        .flatten()
                        .is_none()
                })
                .map(|folder| admit_root(folder.root(), installation.root()))
                .collect::<Result<Vec<_>, _>>()?,
        );
        let runtime = Arc::new(
            RcloneRuntime::prepare(
                Arc::new(
                    guardian
                        .try_clone()
                        .map_err(|_| EngineSessionError::LaunchFailed)?,
                ),
                Arc::new(
                    engine
                        .try_clone()
                        .map_err(|_| EngineSessionError::LaunchFailed)?,
                ),
                runtime_parent,
                &runtime_settings,
                Arc::downgrade(&worker_lease),
                Arc::clone(&installation),
                Arc::clone(&roots),
            )
            .await?,
        );
        Ok(Self {
            worker: None,
            guardian: Arc::new(
                guardian
                    .try_clone()
                    .map_err(|_| EngineSessionError::LaunchFailed)?,
            ),
            runtime,
            installation,
            worker_lease: Some(worker_lease),
            roots,
            settings,
            unavailable_folders,
            jobs: BTreeMap::new(),
            completed_peers: BTreeSet::new(),
            closed: false,
        })
    }

    pub(super) fn begin_initial_scan(&self) -> InitialScanTask {
        let runtime = Arc::clone(&self.runtime);
        let installation = Arc::clone(&self.installation);
        let roots = Arc::clone(&self.roots);
        let worker_lease = self.worker_lease.clone();
        InitialScanTask::new(tokio::spawn(async move {
            let _worker_lease = worker_lease;
            revalidate(&installation, &roots)?;
            runtime.initial_scan().await?;
            revalidate(&installation, &roots)
        }))
    }

    pub(super) async fn reset_folder_index(
        &mut self,
        folder: Uuid,
    ) -> Result<(), EngineSessionError> {
        self.revalidate_roots()?;
        self.runtime.reset_folder(folder)
    }

    pub(super) async fn promote_after_initial_scan(&mut self) -> Result<(), EngineSessionError> {
        self.revalidate_roots()?;
        let has_source = self.settings.folders.iter().any(|folder| {
            matches!(folder.role(), EngineFolderRole::Source)
                && !self.unavailable_folders.contains(&folder.id())
        });
        if !has_source {
            return Ok(());
        }
        let listener = self
            .settings
            .listener
            .ok_or(EngineSessionError::InvalidConfiguration)?;
        let (args, mut environment) = self.runtime.server_inputs(listener)?;
        environment.extend([
            ("RCLONE_CONFIG".into(), "/dev/null".into()),
            (
                "RCLONE_CACHE_DIR".into(),
                self.runtime.runtime_path().join("cache").into_os_string(),
            ),
            (
                "RCLONE_TEMP_DIR".into(),
                self.runtime.runtime_path().as_os_str().to_owned(),
            ),
        ]);
        let keepalive = ServerKeepalive {
            _runtime: Arc::clone(&self.runtime),
            _installation: Arc::clone(&self.installation),
            _roots: Arc::clone(&self.roots),
            _worker_lease: self
                .worker_lease
                .as_ref()
                .cloned()
                .ok_or(EngineSessionError::LaunchFailed)?,
        };
        let worker = OwnedEngineWorker::launch_rclone(
            &self.guardian,
            self.runtime.executable(),
            &args,
            &environment,
            self.runtime.runtime_path(),
            Box::new(keepalive),
        )
        .map_err(|_| EngineSessionError::LaunchFailed)?;
        self.worker = Some(worker);
        let ready = async {
            loop {
                if self
                    .worker
                    .as_mut()
                    .ok_or(EngineSessionError::LaunchFailed)?
                    .try_status()
                    .map_err(|_| EngineSessionError::LaunchFailed)?
                    .is_some()
                {
                    return Err(EngineSessionError::LaunchFailed);
                }
                if tokio::net::TcpStream::connect(listener).await.is_ok() {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::time::timeout(STARTUP_TIMEOUT, ready)
            .await
            .map_err(|_| EngineSessionError::StartupTimeout)?
    }

    pub async fn status(&mut self) -> Result<Value, EngineSessionError> {
        self.revalidate_roots()?;
        Ok(json!({"state": if self.closed { "stopped" } else { "running" }}))
    }

    pub async fn folder_health(&mut self) -> Result<Vec<FolderHealth>, EngineSessionError> {
        self.revalidate_roots()?;
        let now = OffsetDateTime::now_utc();
        Ok(self
            .settings
            .folders
            .iter()
            .map(|folder| FolderHealth {
                folder: folder.id(),
                // An absent runtime-only capability is a per-folder state. It
                // stays visible without converting a healthy sibling into a
                // worker-wide reported error.
                lifecycle: if self.unavailable_folders.contains(&folder.id()) {
                    FolderLifecycle::Error
                } else {
                    FolderLifecycle::Idle
                },
                state_changed: now,
                remaining_files: 0,
                remaining_bytes: 0,
                scan_pull_error_count: 0,
                reported_error_rows: 0,
                status_error: false,
                watch_error: false,
            })
            .collect())
    }

    pub async fn health_observation(
        &mut self,
    ) -> (
        Result<Vec<FolderHealth>, EngineSessionError>,
        Result<Vec<EnginePeerConnection>, EngineSessionError>,
    ) {
        let health = self.folder_health().await;
        let peers = self
            .settings
            .peers
            .iter()
            .map(|peer| {
                EnginePeerConnection::new_runtime(
                    peer.id().clone(),
                    if peer.paused() {
                        EnginePeerConnectionState::Paused
                    } else if self.completed_peers.contains(peer.id()) {
                        EnginePeerConnectionState::Connected
                    } else {
                        EnginePeerConnectionState::Disconnected
                    },
                )
            })
            .collect();
        (health, Ok(peers))
    }

    pub(super) async fn run_observation(
        &mut self,
        folder: Uuid,
        expected: Option<&EngineIndexSnapshot>,
    ) -> Result<super::run_observation::EngineRunObservation, EngineSessionError> {
        self.revalidate_roots()?;
        if self.unavailable_folders.contains(&folder) {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        let role = self
            .settings
            .folders
            .iter()
            .find(|candidate| candidate.id() == folder)
            .ok_or(EngineSessionError::InvalidConfiguration)?
            .role();
        if matches!(role, EngineFolderRole::Source) {
            return Ok(super::run_observation::EngineRunObservation {
                local_index: Some(self.runtime.source_index(folder).await?),
                completions: BTreeMap::new(),
                failures: BTreeSet::new(),
            });
        }
        let expected = expected.ok_or(EngineSessionError::InvalidConfiguration)?;
        if self.jobs.get(&folder).is_some_and(JoinHandle::is_finished) {
            let result = self
                .jobs
                .remove(&folder)
                .ok_or(EngineSessionError::EngineUnavailable)?
                .await
                .map_err(|_| EngineSessionError::EngineUnavailable)?;
            let source = self
                .settings
                .folders
                .iter()
                .find(|candidate| candidate.id() == folder)
                .and_then(|candidate| {
                    candidate
                        .members()
                        .iter()
                        .find(|id| self.settings.peers.iter().any(|peer| peer.id() == *id))
                })
                .cloned()
                .ok_or(EngineSessionError::InvalidConfiguration)?;
            return match result {
                Ok(index) => {
                    self.completed_peers.insert(source.clone());
                    Ok(super::run_observation::EngineRunObservation {
                        local_index: None,
                        completions: BTreeMap::from([(source, index)]),
                        failures: BTreeSet::new(),
                    })
                }
                Err(EngineSessionError::TransferFailed) => {
                    Ok(super::run_observation::EngineRunObservation {
                        local_index: None,
                        completions: BTreeMap::new(),
                        failures: BTreeSet::from([source]),
                    })
                }
                Err(error) => Err(error),
            };
        }
        if !self.jobs.contains_key(&folder) {
            let runtime = Arc::clone(&self.runtime);
            let expected = expected.clone();
            self.jobs.insert(
                folder,
                tokio::spawn(async move { runtime.transfer_destination(folder, &expected).await }),
            );
        }
        Ok(super::run_observation::EngineRunObservation::default())
    }

    pub async fn stop(&mut self) -> Result<StopOutcome, super::EngineSupervisorError> {
        self.close_lifeline();
        for (_, task) in std::mem::take(&mut self.jobs) {
            let _ = task.await;
        }
        let status = if let Some(worker) = &mut self.worker {
            match worker.stop().await? {
                StopOutcome::Exited(status) => status,
                StopOutcome::StillStopping => return Ok(StopOutcome::StillStopping),
            }
        } else {
            ExitStatus::from_raw(0)
        };
        // Command reapers retain the same lease after their tasks are cancelled.
        // Keep the existing stopping lifecycle until no command can still write.
        if self
            .worker_lease
            .as_ref()
            .is_some_and(|lease| Arc::strong_count(lease) > 1)
        {
            return Ok(StopOutcome::StillStopping);
        }
        self.worker_lease.take();
        Ok(StopOutcome::Exited(status))
    }

    pub fn close_lifeline(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        for task in self.jobs.values() {
            task.abort();
        }
        if let Some(worker) = &mut self.worker {
            worker.close_lifeline();
        }
    }

    fn revalidate_roots(&self) -> Result<(), EngineSessionError> {
        revalidate(&self.installation, &self.roots)
    }
}

fn available_runtime_settings(
    settings: &EngineSessionSettings,
    grants: &AndroidSafGrantRegistry,
) -> Result<(EngineSessionSettings, BTreeSet<Uuid>), EngineSessionError> {
    let mut runtime = settings.clone();
    runtime.folders.clear();
    let mut unavailable = BTreeSet::new();
    for folder in &settings.folders {
        match super::android_saf::parse_token(folder.root())
            .map_err(|_| EngineSessionError::InvalidConfiguration)?
        {
            Some(_) => match grants
                .contains_token(folder.root())
                .map_err(|_| EngineSessionError::RuntimeUnavailable)?
            {
                true => runtime.folders.push(folder.clone()),
                false => {
                    unavailable.insert(folder.id());
                }
            },
            None => runtime.folders.push(folder.clone()),
        }
    }
    if runtime.folders.is_empty() {
        runtime.listener = None;
    }
    Ok((runtime, unavailable))
}

impl Drop for ManagedEngineSession {
    fn drop(&mut self) {
        self.close_lifeline();
    }
}

fn admit_root(path: &Path, installation: &Path) -> Result<FolderRootLease, EngineSessionError> {
    if path.starts_with(installation) || installation.starts_with(path) {
        return Err(EngineSessionError::InvalidConfiguration);
    }
    let file = File::open(path).map_err(|_| EngineSessionError::InvalidConfiguration)?;
    let metadata = file
        .metadata()
        .map_err(|_| EngineSessionError::InvalidConfiguration)?;
    let current =
        std::fs::symlink_metadata(path).map_err(|_| EngineSessionError::InvalidConfiguration)?;
    if !metadata.is_dir()
        || !current.is_dir()
        || (metadata.dev(), metadata.ino()) != (current.dev(), current.ino())
    {
        return Err(EngineSessionError::InvalidConfiguration);
    }
    Ok(FolderRootLease {
        path: path.to_path_buf(),
        file,
        identity: (metadata.dev(), metadata.ino()),
    })
}

fn revalidate(
    installation: &EngineInstallation,
    roots: &[FolderRootLease],
) -> Result<(), EngineSessionError> {
    installation
        .revalidate()
        .map_err(|_| EngineSessionError::InvalidConfiguration)?;
    for root in roots {
        let held = root
            .file
            .metadata()
            .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        let current = std::fs::symlink_metadata(&root.path)
            .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        if !held.is_dir()
            || !current.is_dir()
            || (held.dev(), held.ino()) != root.identity
            || (current.dev(), current.ino()) != root.identity
        {
            return Err(EngineSessionError::InvalidConfiguration);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_saf_grant_does_not_remove_an_available_folder() {
        let local = tempfile::tempdir().unwrap();
        let local_id = Uuid::new_v4();
        let missing_id = Uuid::new_v4();
        let settings = EngineSessionSettings {
            device_name: "test".to_owned(),
            listener: Some("127.0.0.1:22000".parse().unwrap()),
            peers: Vec::new(),
            folders: vec![
                EngineFolderConfig::new(local_id, "local", local.path().to_path_buf(), Vec::new())
                    .unwrap(),
                EngineFolderConfig::new(
                    missing_id,
                    "missing",
                    AndroidSafGrantRegistry::token(missing_id),
                    Vec::new(),
                )
                .unwrap(),
            ],
        };

        let (runtime, unavailable) =
            available_runtime_settings(&settings, &AndroidSafGrantRegistry::default()).unwrap();

        assert_eq!(runtime.folders.len(), 1);
        assert_eq!(runtime.folders[0].id(), local_id);
        assert_eq!(runtime.listener, settings.listener);
        assert_eq!(unavailable, BTreeSet::from([missing_id]));
    }
}
