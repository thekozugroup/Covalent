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
use tokio::sync::Notify;
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
    FolderUnavailable,
    LaunchFailed,
    StartupTimeout,
    StartupMismatch,
    EngineUnavailable,
    TransferFailed,
    PendingCopyRecoveryRequired,
}

impl fmt::Display for EngineSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "folder sync configuration is invalid",
            Self::RuntimeUnavailable => "folder sync runtime storage is unavailable",
            Self::FolderUnavailable => "folder access is unavailable",
            Self::LaunchFailed => "folder sync worker could not start",
            Self::StartupTimeout => "folder sync worker did not become ready",
            Self::StartupMismatch => "folder sync worker failed startup verification",
            Self::EngineUnavailable => "folder sync worker is unavailable",
            Self::TransferFailed => "folder transfer did not complete",
            Self::PendingCopyRecoveryRequired => "an interrupted folder copy requires recovery",
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
    progress: Option<Arc<Notify>>,
    closed: bool,
}

fn spawn_with_progress<T: Send + 'static>(
    progress: Option<Arc<Notify>>,
    work: impl std::future::Future<Output = T> + Send + 'static,
) -> JoinHandle<T> {
    tokio::spawn(async move {
        let result = work.await;
        if let Some(progress) = progress {
            progress.notify_one();
        }
        result
    })
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
                &BTreeSet::new(),
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
            progress: None,
            closed: false,
        })
    }

    /// Replace receiver configuration without cancelling unchanged copy jobs.
    pub(super) async fn update_destinations(
        &mut self,
        settings: EngineSessionSettings,
        unchanged_runs: &BTreeSet<Uuid>,
    ) -> Result<bool, EngineSessionError> {
        if self.closed
            || self.worker.is_some()
            || settings.folders.is_empty()
            || self
                .settings
                .folders
                .iter()
                .chain(&settings.folders)
                .any(|folder| !matches!(folder.role(), EngineFolderRole::Destination { .. }))
        {
            return Ok(false);
        }
        self.revalidate_roots()?;
        let (runtime_settings, unavailable) =
            available_runtime_settings(&settings, &self.runtime.executable().android_saf_grants())?;
        for folder_id in self.jobs.keys() {
            let old = self
                .settings
                .folders
                .iter()
                .find(|folder| folder.id() == *folder_id);
            let current = runtime_settings
                .folders
                .iter()
                .find(|folder| folder.id() == *folder_id);
            if !unchanged_runs.contains(folder_id) || old.is_none() || old != current {
                return Ok(false);
            }
            if old.unwrap().members().iter().any(|peer| {
                self.settings
                    .peers
                    .iter()
                    .find(|candidate| candidate.id() == peer)
                    != settings
                        .peers
                        .iter()
                        .find(|candidate| candidate.id() == peer)
            }) {
                return Ok(false);
            }
        }
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
                .map(|folder| admit_root(folder.root(), self.installation.root()))
                .collect::<Result<Vec<_>, _>>()?,
        );
        let lease = self
            .worker_lease
            .as_ref()
            .ok_or(EngineSessionError::LaunchFailed)?;
        let runtime_parent = self
            .runtime
            .runtime_path()
            .parent()
            .ok_or(EngineSessionError::RuntimeUnavailable)?;
        let runtime = RcloneRuntime::prepare(
            Arc::clone(&self.guardian),
            Arc::new(
                self.runtime
                    .executable()
                    .try_clone()
                    .map_err(|_| EngineSessionError::LaunchFailed)?,
            ),
            runtime_parent,
            &runtime_settings,
            Arc::downgrade(lease),
            Arc::clone(&self.installation),
            Arc::clone(&roots),
            &self.jobs.keys().copied().collect(),
        )
        .await?;
        // Old jobs retain their exact runtime, root/SAF grants and worker lease.
        self.runtime = Arc::new(runtime);
        self.roots = roots;
        self.settings = settings;
        self.unavailable_folders = unavailable;
        self.completed_peers.retain(|peer| {
            self.settings
                .peers
                .iter()
                .any(|candidate| candidate.id() == peer)
        });
        Ok(true)
    }

    pub(super) fn set_progress_signal(&mut self, progress: Arc<Notify>) {
        self.progress = Some(progress);
    }

    pub(super) fn begin_initial_scan(&self) -> InitialScanTask {
        let runtime = Arc::clone(&self.runtime);
        let installation = Arc::clone(&self.installation);
        let roots = Arc::clone(&self.roots);
        let worker_lease = self.worker_lease.clone();
        InitialScanTask::new(spawn_with_progress(self.progress.clone(), async move {
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
            .map(|folder| {
                let access_unavailable = self.unavailable_folders.contains(&folder.id());
                FolderHealth {
                    folder: folder.id(),
                    // An absent runtime-only capability is a per-folder state. It
                    // stays visible without converting a healthy sibling into a
                    // worker-wide reported error.
                    lifecycle: if access_unavailable {
                        FolderLifecycle::Error
                    } else {
                        FolderLifecycle::Idle
                    },
                    access_unavailable,
                    state_changed: now,
                    remaining_files: 0,
                    remaining_bytes: 0,
                    scan_pull_error_count: 0,
                    reported_error_rows: 0,
                    status_error: false,
                    watch_error: false,
                }
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
            return Err(EngineSessionError::FolderUnavailable);
        }
        let role = self
            .settings
            .folders
            .iter()
            .find(|candidate| candidate.id() == folder)
            .ok_or(EngineSessionError::InvalidConfiguration)?
            .role();
        if matches!(role, EngineFolderRole::Source) {
            if self.jobs.get(&folder).is_some_and(JoinHandle::is_finished) {
                let index = self
                    .jobs
                    .remove(&folder)
                    .expect("finished inventory checked")
                    .await
                    .map_err(|_| EngineSessionError::EngineUnavailable)??;
                return Ok(super::run_observation::EngineRunObservation {
                    local_index: Some(index),
                    ..Default::default()
                });
            }
            // Hashing a large source must not hold the service mutation lock.
            // The session owns and reaps this job just like destination copies.
            if !self.jobs.contains_key(&folder) {
                let runtime = Arc::clone(&self.runtime);
                self.jobs.insert(
                    folder,
                    spawn_with_progress(self.progress.clone(), async move {
                        runtime.source_index(folder).await
                    }),
                );
            }
            return Ok(super::run_observation::EngineRunObservation::default());
        }
        let expected = expected.ok_or(EngineSessionError::InvalidConfiguration)?;
        if self.jobs.get(&folder).is_some_and(JoinHandle::is_finished) {
            let result = self
                .jobs
                .remove(&folder)
                .ok_or(EngineSessionError::EngineUnavailable)?
                .await
                .unwrap_or(Err(EngineSessionError::EngineUnavailable));
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
                // A completed attempt has failed. Preserve its recovery state
                // and report failure instead of silently restarting the job.
                Err(error) => {
                    super::rclone::report_transfer_error("destination", error);
                    Ok(super::run_observation::EngineRunObservation {
                        local_index: None,
                        completions: BTreeMap::new(),
                        failures: BTreeSet::from([source]),
                    })
                }
            };
        }
        if !self.jobs.contains_key(&folder) {
            let runtime = Arc::clone(&self.runtime);
            let expected = expected.clone();
            self.jobs.insert(
                folder,
                spawn_with_progress(self.progress.clone(), async move {
                    runtime.transfer_destination(folder, &expected).await
                }),
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

    #[tokio::test]
    async fn receiver_updates_keep_unchanged_jobs_and_reject_authority_changes() {
        use sha2::{Digest as _, Sha256};
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let state = root.join("state");
        let first_root = root.join("first");
        let second_root = root.join("second");
        for path in [&state, &first_root, &second_root] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let installation = Arc::new(
            EngineInstallation::create(
                &state,
                &covalent_core::StaticKeyProtector::new(1, [9; 32]).unwrap(),
            )
            .unwrap(),
        );
        let worker_path = root.join("worker");
        let script = b"#!/bin/sh\nexit 1\n";
        std::fs::write(&worker_path, script).unwrap();
        std::fs::set_permissions(&worker_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let worker =
            VerifiedEngineExecutable::open(&worker_path, Sha256::digest(script).into()).unwrap();
        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let destination = EngineFolderRole::Destination {
            propagate_source_deletions: false,
            restore_local_deletions: false,
        };
        let first = EngineFolderConfig::new(first_id, "first", first_root.clone(), Vec::new())
            .unwrap()
            .with_role(destination);
        let second = EngineFolderConfig::new(second_id, "second", second_root, Vec::new())
            .unwrap()
            .with_role(destination);
        let initial = EngineSessionSettings {
            device_name: "receiver".into(),
            listener: None,
            peers: Vec::new(),
            folders: vec![first.clone()],
        };
        let mut session =
            ManagedEngineSession::start(installation, &worker, &worker, &root, initial.clone())
                .await
                .unwrap();
        let (complete, pending) = tokio::sync::oneshot::channel();
        let lease = session.worker_lease.as_ref().unwrap().clone();
        session.jobs.insert(
            first_id,
            tokio::spawn(async move {
                let _lease = lease;
                pending.await.unwrap()
            }),
        );
        let job = session.jobs[&first_id].id();
        let staged = session
            .installation
            .database_directory()
            .unwrap()
            .join(format!("rclone-policy-{first_id}.staged.v1.json"));
        std::fs::write(&staged, b"in-flight atomic policy staging").unwrap();
        let mut expanded = initial.clone();
        expanded.folders.push(second);
        let unchanged = BTreeSet::from([first_id]);
        assert!(
            session
                .update_destinations(expanded.clone(), &unchanged)
                .await
                .unwrap()
        );
        assert_eq!(session.jobs[&first_id].id(), job);
        assert!(!session.jobs[&first_id].is_finished());
        assert_eq!(session.settings.folders.len(), 2);
        assert!(
            !session
                .update_destinations(expanded.clone(), &BTreeSet::new())
                .await
                .unwrap(),
            "a newer generation cannot inherit an old copy"
        );
        let mut changed = expanded.clone();
        changed.folders[0] = first.clone().with_role(EngineFolderRole::Source);
        assert!(
            !session
                .update_destinations(changed, &unchanged)
                .await
                .unwrap()
        );
        let mut changed = expanded.clone();
        changed.folders[0] =
            EngineFolderConfig::new(first_id, "first", root.join("second"), Vec::new())
                .unwrap()
                .with_role(destination);
        assert!(
            !session
                .update_destinations(changed, &unchanged)
                .await
                .unwrap(),
            "active root replacement needs the stop boundary"
        );
        let mut changed = expanded;
        changed.folders.retain(|folder| folder.id() != first_id);
        assert!(
            !session
                .update_destinations(changed, &unchanged)
                .await
                .unwrap(),
            "revocation of an active folder must stop it"
        );
        assert!(
            session
                .update_destinations(initial.clone(), &unchanged)
                .await
                .unwrap(),
            "removing an idle unrelated destination must preserve the active copy"
        );
        assert_eq!(session.jobs[&first_id].id(), job);
        let index = EngineIndexSnapshot {
            index_id: "0x0123456789ABCDEF".into(),
            sequence: 1,
        };
        complete.send(Ok(index.clone())).unwrap();
        assert_eq!(
            session
                .jobs
                .remove(&first_id)
                .unwrap()
                .await
                .unwrap()
                .unwrap(),
            index
        );
        assert_eq!(
            std::fs::read(&staged).unwrap(),
            b"in-flight atomic policy staging"
        );
        assert!(
            session
                .update_destinations(initial, &BTreeSet::new())
                .await
                .is_err(),
            "without an owning job, staged policy still refuses a fresh runtime"
        );
        std::fs::remove_file(staged).unwrap();
        assert!(matches!(
            session.stop().await.unwrap(),
            StopOutcome::Exited(_)
        ));
    }

    #[tokio::test]
    async fn background_work_signals_only_completed_success_or_failure() {
        for result in [Ok(()), Err(EngineSessionError::TransferFailed)] {
            let progress = Arc::new(Notify::new());
            let (complete, pending) = tokio::sync::oneshot::channel();
            let job = spawn_with_progress(Some(Arc::clone(&progress)), async move {
                pending.await.unwrap()
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(25), progress.notified())
                    .await
                    .is_err(),
                "pending work must not announce completion"
            );
            complete.send(result).unwrap();
            tokio::time::timeout(Duration::from_secs(1), progress.notified())
                .await
                .expect("both success and failure must wake their owner");
            assert_eq!(job.await.unwrap(), result);
        }
        let progress = Arc::new(Notify::new());
        let job = spawn_with_progress(Some(Arc::clone(&progress)), std::future::pending::<()>());
        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(Duration::from_millis(25), progress.notified())
                .await
                .is_err(),
            "cancelled work must not announce a completed result"
        );
    }

    #[tokio::test]
    async fn source_inventory_is_polled_without_waiting_and_stop_drains_it() {
        use sha2::{Digest as _, Sha256};
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let state = root.join("state");
        let files = root.join("files");
        for path in [&state, &files] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let installation = Arc::new(
            EngineInstallation::create(
                &state,
                &covalent_core::StaticKeyProtector::new(1, [7; 32]).unwrap(),
            )
            .unwrap(),
        );
        // Preparation verifies the executable; this test controls jobs before
        // they launch any process, so it needs no installed rclone or guardian.
        let executable = root.join("worker");
        let script = b"#!/bin/sh\nexit 1\n";
        std::fs::write(&executable, script).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executable =
            VerifiedEngineExecutable::open(&executable, Sha256::digest(script).into()).unwrap();
        let folder = Uuid::new_v4();
        let mut session = ManagedEngineSession::start(
            Arc::clone(&installation),
            &executable,
            &executable,
            &root,
            EngineSessionSettings {
                device_name: "source".into(),
                listener: None,
                peers: Vec::new(),
                folders: vec![
                    EngineFolderConfig::new(folder, "files", files, Vec::new())
                        .unwrap()
                        .with_role(EngineFolderRole::Source),
                ],
            },
        )
        .await
        .unwrap();
        assert_eq!(
            session.run_observation(folder, None).await.unwrap(),
            Default::default()
        );
        let launched = session
            .jobs
            .remove(&folder)
            .expect("inventory runs in background");
        launched.abort();
        assert!(launched.await.unwrap_err().is_cancelled());

        let index = EngineIndexSnapshot {
            index_id: "0x0123456789ABCDEF".into(),
            sequence: 1,
        };
        let (complete, pending) = tokio::sync::oneshot::channel();
        session.jobs.insert(
            folder,
            tokio::spawn(async move { Ok(pending.await.unwrap()) }),
        );
        let task_id = session.jobs[&folder].id();
        for _ in 0..2 {
            let observed = tokio::time::timeout(
                Duration::from_millis(100),
                session.run_observation(folder, None),
            )
            .await
            .expect("pending inventory must not block status and consent")
            .unwrap();
            assert_eq!(observed, Default::default());
            assert_eq!(
                session.jobs[&folder].id(),
                task_id,
                "do not restart an active inventory"
            );
        }
        complete.send(index.clone()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !session.jobs[&folder].is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            session
                .run_observation(folder, None)
                .await
                .unwrap()
                .local_index,
            Some(index)
        );
        assert!(session.jobs.is_empty());

        let lease = session.worker_lease.as_ref().unwrap().clone();
        session.jobs.insert(
            folder,
            tokio::spawn(async move {
                let _lease = lease;
                std::future::pending::<Result<EngineIndexSnapshot, EngineSessionError>>().await
            }),
        );
        tokio::task::yield_now().await;
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), session.stop())
                .await
                .unwrap()
                .unwrap(),
            StopOutcome::Exited(_)
        ));
        assert!(session.jobs.is_empty());
        assert!(
            installation.claim_worker().is_ok(),
            "cancelled inventory must release its worker lease"
        );
    }

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
