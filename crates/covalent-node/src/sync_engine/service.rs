//! Serialized durable consent and owned worker lifecycle.
//!
//! This service is the only boundary that combines folder-sharing journal
//! mutations with worker replacement. A signed decision is returned only after
//! the journal made it durable. Worker failure after that point is reported in
//! the returned lifecycle and never rolls the decision back.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use covalent_core::Engine;
use covalent_protocol::{
    DeviceId, FolderShareAcceptance, FolderShareCommit, FolderShareOffer, PeerGrant, SignedRoster,
    SyncEngineBinding, TransportBinding,
};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::controller::InitialScanTask;
use super::{
    EngineInstallation, EnginePeerConnection, EnginePeerConnectionState, EngineSessionError,
    EngineSessionSettings, FolderHealth, FolderSharingJournal, ManagedEngineSession, ShareSummary,
    SharingError, StopOutcome, VerifiedEngineExecutable,
};

const HEALTH_INTERVAL: Duration = Duration::from_secs(5);

/// Stable reason that a durable service needs an explicit retry or repair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderSyncIssue {
    Journal,
    WorkerLaunch,
    InitialScan,
    WorkerHealth,
    WorkerStop,
    PeerRevocation,
}

/// Current worker lifecycle. `InitialScanning` is network-inert and this enum
/// never claims that folders are up to date.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderSyncLifecycle {
    Stopped,
    InitialScanning,
    Running,
    StillStopping,
    NeedsAttention(FolderSyncIssue),
}

/// Whether cached folder observations describe the currently running worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderHealthFreshness {
    NeverObserved,
    Fresh,
    Stale,
}

/// Whether the cached peer connection observation describes the current
/// running worker. Stale observations never retain a connected claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerConnectionFreshness {
    NeverObserved,
    Fresh,
    Stale,
}

/// Redacted connection state for one authenticated Covalent peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerConnectionState {
    Unknown,
    Connected,
    Disconnected,
    Paused,
}

/// A pre-commit service failure. Post-commit failures are represented by
/// [`CommittedMutation`] so durable signatures and decisions are not hidden.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderSyncServiceError {
    Busy,
    InvalidConfiguration,
    Journal,
    WorkerLaunch,
    WorkerStop,
    WorkerStillStopping,
    SettingsConflict,
    SettingsPending,
    RunConflict,
    RunPending,
}

impl fmt::Display for FolderSyncServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Busy => "folder sync is busy",
            Self::InvalidConfiguration => "folder sync service configuration is invalid",
            Self::Journal => "folder sync consent state is unavailable",
            Self::WorkerLaunch => "folder sync worker could not start",
            Self::WorkerStop => "folder sync worker could not be reaped",
            Self::WorkerStillStopping => "folder sync worker is still stopping",
            Self::SettingsConflict => "link settings changed on another device",
            Self::SettingsPending => "a link settings change is waiting for the source",
            Self::RunConflict => "the link run changed; refresh before starting another run",
            Self::RunPending => "a run request is waiting for the source",
        })
    }
}

impl std::error::Error for FolderSyncServiceError {}

/// A value whose associated local journal decision is already durable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedMutation<T> {
    value: T,
    lifecycle: FolderSyncLifecycle,
}

impl<T> CommittedMutation<T> {
    #[must_use]
    pub fn value(&self) -> &T {
        &self.value
    }

    #[must_use]
    pub fn into_value(self) -> T {
        self.value
    }

    #[must_use]
    pub fn lifecycle(&self) -> FolderSyncLifecycle {
        self.lifecycle
    }
}

/// Secret-free service snapshot. Share phases describe consent, not transfer
/// completion; the worker lifecycle describes only process readiness.
#[derive(Clone, Debug)]
pub struct FolderSyncStatus {
    lifecycle: FolderSyncLifecycle,
    shares: Vec<ShareSummary>,
    folder_health: Vec<FolderHealth>,
    health_freshness: FolderHealthFreshness,
    peer_connections: BTreeMap<DeviceId, PeerConnectionState>,
    connection_freshness: PeerConnectionFreshness,
}

impl FolderSyncStatus {
    #[must_use]
    pub fn lifecycle(&self) -> FolderSyncLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub fn shares(&self) -> &[ShareSummary] {
        &self.shares
    }

    /// Last bounded per-folder observation. It never implies that an initial
    /// scan completed or that a peer received the same bytes.
    #[must_use]
    pub fn folder_health(&self) -> &[FolderHealth] {
        &self.folder_health
    }

    #[must_use]
    pub fn health_freshness(&self) -> FolderHealthFreshness {
        self.health_freshness
    }

    /// Return the fresh redacted state for one Covalent peer. An absent or
    /// stale observation is always unknown.
    #[must_use]
    pub fn peer_connection(&self, peer: DeviceId) -> PeerConnectionState {
        if self.connection_freshness != PeerConnectionFreshness::Fresh {
            return PeerConnectionState::Unknown;
        }
        self.peer_connections
            .get(&peer)
            .copied()
            .unwrap_or(PeerConnectionState::Unknown)
    }

    #[must_use]
    pub fn connection_freshness(&self) -> PeerConnectionFreshness {
        self.connection_freshness
    }
}

struct ProductionLauncher {
    installation: Arc<EngineInstallation>,
    guardian: VerifiedEngineExecutable,
    engine: VerifiedEngineExecutable,
    runtime_parent: PathBuf,
}

enum Launcher {
    Production(ProductionLauncher),
    #[cfg(test)]
    Test(Arc<TestBackend>),
}

enum Session {
    Production(Box<ManagedEngineSession>),
    #[cfg(test)]
    Test(TestSession),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionStop {
    Exited,
    StillStopping,
}

impl Launcher {
    fn validate_selected_root(&self, selected_root: &Path) -> Result<(), FolderSyncServiceError> {
        let Some(_) = super::android_saf::parse_token(selected_root)
            .map_err(|_| FolderSyncServiceError::InvalidConfiguration)?
        else {
            return Ok(());
        };
        match self {
            Self::Production(launcher) => launcher
                .engine
                .android_saf_grants()
                .contains_token(selected_root)
                .map_err(|_| FolderSyncServiceError::InvalidConfiguration)
                .and_then(|present| {
                    present
                        .then_some(())
                        .ok_or(FolderSyncServiceError::InvalidConfiguration)
                }),
            #[cfg(test)]
            Self::Test(_) => Err(FolderSyncServiceError::InvalidConfiguration),
        }
    }

    async fn launch(
        &self,
        settings: EngineSessionSettings,
        reset_gate: bool,
    ) -> Result<Session, EngineSessionError> {
        match self {
            Self::Production(launcher) => {
                if reset_gate {
                    ManagedEngineSession::start_reset_gate(
                        Arc::clone(&launcher.installation),
                        &launcher.guardian,
                        &launcher.engine,
                        &launcher.runtime_parent,
                        settings,
                    )
                    .await
                } else {
                    ManagedEngineSession::start(
                        Arc::clone(&launcher.installation),
                        &launcher.guardian,
                        &launcher.engine,
                        &launcher.runtime_parent,
                        settings,
                    )
                    .await
                }
            }
            .map(Box::new)
            .map(Session::Production),
            #[cfg(test)]
            Self::Test(backend) => backend
                .launch(settings, reset_gate)
                .await
                .map(Session::Test),
        }
    }
}

impl Session {
    async fn run_observation(
        &mut self,
        folder: Uuid,
        expected: Option<&super::EngineIndexSnapshot>,
    ) -> Result<super::run_observation::EngineRunObservation, RunObservationError> {
        match self {
            Self::Production(session) => {
                session
                    .run_observation(folder, expected)
                    .await
                    .map_err(|error| match error {
                        EngineSessionError::FolderUnavailable => {
                            RunObservationError::FolderUnavailable
                        }
                        EngineSessionError::PendingCopyRecoveryRequired => {
                            RunObservationError::PendingCopyRecoveryRequired
                        }
                        _ => RunObservationError::Retryable,
                    })
            }
            #[cfg(test)]
            Self::Test(session) => session.run_observation(folder),
        }
    }

    fn begin_initial_scan(&self) -> InitialScanTask {
        match self {
            Self::Production(session) => session.begin_initial_scan(),
            #[cfg(test)]
            Self::Test(session) => session.begin_initial_scan(),
        }
    }

    async fn promote_after_initial_scan(&mut self) -> Result<(), ()> {
        match self {
            Self::Production(session) => session.promote_after_initial_scan().await.map_err(|_| ()),
            #[cfg(test)]
            Self::Test(session) => session.promote_after_initial_scan().await,
        }
    }

    async fn reset_folder_index(&mut self, folder: Uuid) -> Result<(), ()> {
        match self {
            Self::Production(session) => session.reset_folder_index(folder).await.map_err(|_| ()),
            #[cfg(test)]
            Self::Test(session) => session.reset_folder_index(folder).await,
        }
    }

    fn close_lifeline(&mut self) {
        match self {
            Self::Production(session) => session.close_lifeline(),
            #[cfg(test)]
            Self::Test(session) => session.close_lifeline(),
        }
    }

    async fn stop(&mut self) -> Result<SessionStop, ()> {
        match self {
            Self::Production(session) => session
                .stop()
                .await
                .map(|outcome| match outcome {
                    StopOutcome::Exited(_) => SessionStop::Exited,
                    StopOutcome::StillStopping => SessionStop::StillStopping,
                })
                .map_err(|_| ()),
            #[cfg(test)]
            Self::Test(session) => session.stop().await,
        }
    }

    async fn health(
        &mut self,
    ) -> (
        Result<Vec<FolderHealth>, ()>,
        Result<Vec<EnginePeerConnection>, ()>,
    ) {
        match self {
            Self::Production(session) => {
                let (folders, connections) = session.health_observation().await;
                (folders.map_err(|_| ()), connections.map_err(|_| ()))
            }
            #[cfg(test)]
            Self::Test(session) => session.health().await,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunObservationError {
    FolderUnavailable,
    PendingCopyRecoveryRequired,
    Retryable,
}

struct ServiceInner {
    journal: FolderSharingJournal,
    initial_scan: Option<InitialScanTask>,
    session: Option<Session>,
    applied_settings: Option<EngineSessionSettings>,
    applied_batch_generations: BTreeMap<Uuid, u64>,
    automatic_runs_enabled: bool,
    lifecycle: FolderSyncLifecycle,
    after_stop: FolderSyncLifecycle,
    restart_after_stop: bool,
    folder_health: Vec<FolderHealth>,
    health_freshness: FolderHealthFreshness,
    peer_connections: BTreeMap<DeviceId, PeerConnectionState>,
    connection_freshness: PeerConnectionFreshness,
}

struct ServiceShared {
    inner: Mutex<ServiceInner>,
    launcher: Launcher,
    engine: Arc<Engine>,
}

/// One owner for durable sharing decisions and the exact engine worker.
///
/// Methods intentionally return `Busy` rather than waiting behind another
/// mutation. This lets every mutation close an existing guardian lifeline
/// synchronously before its first asynchronous wait.
pub struct FolderSyncService {
    shared: Arc<ServiceShared>,
    shutdown: watch::Sender<bool>,
    health_task: Option<JoinHandle<()>>,
}

impl fmt::Debug for FolderSyncService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FolderSyncService([PRIVATE])")
    }
}

impl FolderSyncService {
    /// Construct a stopped service. Call [`Self::start`] to reconcile durable
    /// consent and launch only when at least one folder is ready.
    pub fn new(
        journal: FolderSharingJournal,
        installation: Arc<EngineInstallation>,
        engine: Arc<Engine>,
        guardian: VerifiedEngineExecutable,
        worker: VerifiedEngineExecutable,
        runtime_parent: impl Into<PathBuf>,
    ) -> Result<Self, FolderSyncServiceError> {
        if journal.binding().covalent_device_id != engine.device_id()
            || journal.binding().engine_device_id.as_str() != installation.device_id().as_str()
        {
            return Err(FolderSyncServiceError::InvalidConfiguration);
        }
        Self::construct(
            journal,
            engine,
            Launcher::Production(ProductionLauncher {
                installation,
                guardian,
                engine: worker,
                runtime_parent: runtime_parent.into(),
            }),
            HEALTH_INTERVAL,
        )
    }

    fn construct(
        mut journal: FolderSharingJournal,
        engine: Arc<Engine>,
        launcher: Launcher,
        health_interval: Duration,
    ) -> Result<Self, FolderSyncServiceError> {
        if health_interval.is_zero() {
            return Err(FolderSyncServiceError::InvalidConfiguration);
        }
        if cfg!(target_os = "android") {
            journal.set_android_host(true);
        }
        journal
            .interrupt_unfinished_link_runs(crate::now_unix_ms())
            .map_err(map_journal_error)?;
        let (shutdown, receiver) = watch::channel(false);
        let shared = Arc::new(ServiceShared {
            inner: Mutex::new(ServiceInner {
                journal,
                initial_scan: None,
                session: None,
                applied_settings: None,
                applied_batch_generations: BTreeMap::new(),
                automatic_runs_enabled: false,
                lifecycle: FolderSyncLifecycle::Stopped,
                after_stop: FolderSyncLifecycle::Stopped,
                restart_after_stop: false,
                folder_health: Vec::new(),
                health_freshness: FolderHealthFreshness::NeverObserved,
                peer_connections: BTreeMap::new(),
                connection_freshness: PeerConnectionFreshness::NeverObserved,
            }),
            launcher,
            engine,
        });
        let weak = Arc::downgrade(&shared);
        let health_task = tokio::runtime::Handle::try_current()
            .map_err(|_| FolderSyncServiceError::InvalidConfiguration)?
            .spawn(health_loop(weak, receiver, health_interval));
        Ok(Self {
            shared,
            shutdown,
            health_task: Some(health_task),
        })
    }

    /// Reconcile trust and desired state, launching no helper for an empty set.
    pub async fn start(&self) -> Result<FolderSyncLifecycle, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        inner.automatic_runs_enabled = true;
        match inner.journal.pending_peer_address_refresh() {
            Ok(None) => {}
            Ok(Some(_)) | Err(_) => {
                if inner.session.is_some() {
                    let _ = quiesce(
                        &mut inner,
                        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                        false,
                    )
                    .await;
                } else {
                    inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal);
                }
                return Err(FolderSyncServiceError::Journal);
            }
        }
        let (desired, batches) = match inner
            .journal
            .desired_settings_and_runs(crate::now_unix_ms(), Instant::now())
        {
            Ok(desired) => desired,
            Err(error) => {
                if inner.session.is_some() {
                    let _ = quiesce(
                        &mut inner,
                        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                        false,
                    )
                    .await;
                } else {
                    inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal);
                }
                return Err(map_journal_error(error));
            }
        };
        if matches!(
            inner.lifecycle,
            FolderSyncLifecycle::Running | FolderSyncLifecycle::InitialScanning
        ) && inner.applied_settings.as_ref() == Some(&desired)
            && inner.applied_batch_generations == batches
        {
            if inner.lifecycle == FolderSyncLifecycle::InitialScanning {
                advance_initial_scan(&mut inner).await;
            }
            return Ok(inner.lifecycle);
        }
        if inner.session.is_some() {
            quiesce(&mut inner, FolderSyncLifecycle::Stopped, false).await?;
            if let Err(issue) = start_desired(&self.shared, &mut inner).await {
                inner.lifecycle = FolderSyncLifecycle::NeedsAttention(issue);
                return Err(map_start_issue(issue));
            }
            return Ok(inner.lifecycle);
        }
        if let Err(issue) = apply_desired(&self.shared, &mut inner, desired, batches).await {
            inner.lifecycle = FolderSyncLifecycle::NeedsAttention(issue);
            return Err(FolderSyncServiceError::WorkerLaunch);
        }
        Ok(inner.lifecycle)
    }

    /// Stop and reap the exact owned worker. `StillStopping` remains explicit.
    pub async fn stop(&self) -> Result<FolderSyncLifecycle, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        inner.automatic_runs_enabled = false;
        match quiesce(&mut inner, FolderSyncLifecycle::Stopped, false).await {
            Ok(()) => Ok(inner.lifecycle),
            Err(FolderSyncServiceError::WorkerStillStopping) => Ok(inner.lifecycle),
            Err(error) => Err(error),
        }
    }

    pub async fn offer(
        &self,
        peer_id: DeviceId,
        folder_id: Uuid,
        label: &str,
        selected_root: &Path,
        now: u64,
    ) -> Result<CommittedMutation<FolderShareOffer>, FolderSyncServiceError> {
        self.shared.launcher.validate_selected_root(selected_root)?;
        self.mutate(|journal| journal.offer(peer_id, folder_id, label, selected_root, now))
            .await
    }

    pub async fn offer_with_policy(
        &self,
        peer_id: DeviceId,
        folder_id: Uuid,
        label: &str,
        selected_root: &Path,
        now: u64,
        policy: Option<covalent_protocol::FolderLinkPolicy>,
    ) -> Result<CommittedMutation<FolderShareOffer>, FolderSyncServiceError> {
        self.shared.launcher.validate_selected_root(selected_root)?;
        self.mutate(|journal| {
            journal.offer_with_policy(peer_id, folder_id, label, selected_root, now, policy)
        })
        .await
    }

    pub async fn offer_with_settings(
        &self,
        peer_id: DeviceId,
        folder_id: Uuid,
        label: &str,
        selected_root: &Path,
        now: u64,
        settings: super::FolderLinkSettings,
    ) -> Result<CommittedMutation<FolderShareOffer>, FolderSyncServiceError> {
        self.shared.launcher.validate_selected_root(selected_root)?;
        self.mutate(|journal| {
            journal.offer_with_settings(peer_id, folder_id, label, selected_root, now, settings)
        })
        .await
    }

    pub async fn request_link_run(
        &self,
        folder_id: Uuid,
        request_id: Uuid,
        expected_generation: u64,
        settings_revision: u64,
    ) -> Result<CommittedMutation<super::LinkRunAdmission>, FolderSyncServiceError> {
        self.mutate(|journal| {
            journal.request_link_run(
                folder_id,
                request_id,
                expected_generation,
                settings_revision,
                crate::now_unix_ms(),
            )
        })
        .await
    }

    pub async fn receive_link_run_request(
        &self,
        request: &super::LinkRunRequest,
    ) -> Result<CommittedMutation<super::LinkRunCommit>, FolderSyncServiceError> {
        self.mutate(|journal| journal.receive_link_run_request(request, crate::now_unix_ms()))
            .await
    }

    pub async fn receive_link_run_commit(
        &self,
        commit: &super::LinkRunCommit,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| journal.receive_link_run_commit(commit))
            .await
    }

    pub async fn receive_link_run_report(
        &self,
        report: &super::LinkRunReport,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| {
            journal
                .receive_link_run_report(report, crate::now_unix_ms())
                .map(|_| ())
        })
        .await
    }

    pub async fn observe_android_conditions(
        &self,
        wifi_connected: bool,
        charging: bool,
    ) -> Result<FolderSyncLifecycle, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        inner
            .journal
            .observe_android_conditions(wifi_connected, charging, Instant::now());
        // A routine platform observation must not retry a failed worker.
        if !inner.automatic_runs_enabled
            || matches!(inner.lifecycle, FolderSyncLifecycle::NeedsAttention(_))
        {
            return Ok(inner.lifecycle);
        }
        Ok(reconcile_committed(&self.shared, &mut inner).await)
    }

    /// Read durable retransmission records while reconciling any intervening
    /// revocation with the owned worker before returning them to the dialer.
    pub async fn outbound_records(
        &self,
    ) -> Result<CommittedMutation<Vec<super::FolderShareDelivery>>, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        let records = match inner.journal.outbound_records() {
            Ok(records) => records,
            Err(error) => {
                let _ = quiesce(
                    &mut inner,
                    FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                    false,
                )
                .await;
                return Err(map_journal_error(error));
            }
        };
        // Reading the outbox must not restart a worker stopped by a health
        // failure. Only an explicit retry or a new user/peer decision may do
        // that. A running worker still reconciles newly revoked membership.
        if matches!(
            inner.lifecycle,
            FolderSyncLifecycle::Running | FolderSyncLifecycle::InitialScanning
        ) {
            reconcile_committed(&self.shared, &mut inner).await;
        }
        Ok(CommittedMutation {
            value: records,
            lifecycle: inner.lifecycle,
        })
    }

    pub async fn receive_offer(
        &self,
        offer: FolderShareOffer,
        now: u64,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| journal.receive_offer(offer, now))
            .await
    }

    /// Durably replace one expired outgoing invitation with a fresh signed
    /// invitation. No worker starts because renewal carries no target consent.
    pub async fn renew_offer(
        &self,
        offer_id: Uuid,
        now: u64,
    ) -> Result<CommittedMutation<FolderShareOffer>, FolderSyncServiceError> {
        self.mutate(|journal| journal.renew_offer(offer_id, now))
            .await
    }

    pub async fn accept(
        &self,
        offer_id: Uuid,
        selected_root: &Path,
        now: u64,
    ) -> Result<CommittedMutation<FolderShareAcceptance>, FolderSyncServiceError> {
        self.shared.launcher.validate_selected_root(selected_root)?;
        self.mutate(|journal| journal.accept(offer_id, selected_root, now))
            .await
    }

    pub async fn receive_acceptance(
        &self,
        offer_id: Uuid,
        acceptance: FolderShareAcceptance,
        now: u64,
    ) -> Result<CommittedMutation<FolderShareCommit>, FolderSyncServiceError> {
        self.mutate(|journal| journal.receive_acceptance(offer_id, acceptance, now))
            .await
    }

    pub async fn receive_commit(
        &self,
        offer_id: Uuid,
        commit: FolderShareCommit,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| journal.receive_commit(offer_id, commit))
            .await
    }

    /// Apply one peer withdrawal already authenticated by the folder-control
    /// envelope. The peer is not notified again from this node.
    pub async fn receive_removal(
        &self,
        notice: &super::FolderRemovalNotice,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| journal.receive_removal(notice)).await
    }

    pub async fn request_link_settings(
        &self,
        folder_id: Uuid,
        change_id: Uuid,
        expected_revision: u64,
        settings: super::FolderLinkSettings,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| {
            journal.request_link_settings_at(
                folder_id,
                change_id,
                expected_revision,
                settings,
                crate::now_unix_ms(),
            )
        })
        .await
    }

    pub async fn receive_link_settings_request(
        &self,
        request: &super::LinkSettingsRequest,
    ) -> Result<CommittedMutation<super::LinkSettingsCommit>, FolderSyncServiceError> {
        self.mutate(|journal| {
            journal.receive_link_settings_request_at(request, crate::now_unix_ms())
        })
        .await
    }

    pub async fn receive_link_settings_commit(
        &self,
        commit: &super::LinkSettingsCommit,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| journal.receive_link_settings_commit(commit))
            .await
    }

    /// Durably retire a removal outbox entry after an authenticated peer ACK.
    pub(crate) async fn acknowledge_removal(
        &self,
        peer_id: DeviceId,
        current_offer_id: Uuid,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| journal.acknowledge_removal(peer_id, current_offer_id))
            .await
    }

    pub async fn pause(
        &self,
        offer_id: Uuid,
        paused: bool,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| journal.set_paused(offer_id, paused))
            .await
    }

    pub async fn remove(
        &self,
        offer_id: Uuid,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.mutate(|journal| journal.remove(offer_id)).await
    }

    /// Stop and reap the current worker before changing a retained local root.
    /// A committed repair is then reconciled through the ordinary full initial
    /// scan barrier before the replacement worker may become running.
    pub async fn repair_root(
        &self,
        offer_id: Uuid,
        selected_root: &Path,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        self.shared.launcher.validate_selected_root(selected_root)?;
        let mut inner = self.try_inner()?;
        let before_revision = inner.journal.revision();
        let prepared = match inner.journal.prepare_root_repair(offer_id, selected_root) {
            Ok(prepared) => prepared,
            Err(error) => {
                // Trust reconciliation can durably retire a share during
                // preflight. Reconcile that changed desired state; an ordinary
                // invalid root or multi-member move leaves the worker intact.
                if inner.journal.revision() != before_revision {
                    let _ = reconcile_committed(&self.shared, &mut inner).await;
                }
                return Err(map_journal_error(error));
            }
        };
        let repaired_folder = prepared.folder_id();
        quiesce(&mut inner, FolderSyncLifecycle::Stopped, false).await?;
        inner
            .journal
            .commit_root_repair(prepared)
            .map_err(map_journal_error)?;
        inner
            .folder_health
            .retain(|health| health.folder != repaired_folder);
        if inner.folder_health.is_empty() {
            inner.health_freshness = FolderHealthFreshness::NeverObserved;
        }
        let lifecycle = reconcile_committed(&self.shared, &mut inner).await;
        Ok(CommittedMutation {
            value: (),
            lifecycle,
        })
    }

    /// Stop first, persist every share tombstone, revoke Covalent trust, then
    /// launch only the reconciled remainder. `None` means tombstones committed
    /// but peer revocation still needs attention and may be safely retried.
    pub async fn revoke_peer(
        &self,
        peer_id: DeviceId,
    ) -> Result<CommittedMutation<Option<SignedRoster>>, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        quiesce(&mut inner, FolderSyncLifecycle::Stopped, false).await?;
        inner
            .journal
            .remove_peer(peer_id)
            .map_err(map_journal_error)?;
        let roster = match self.shared.engine.revoke_peer(peer_id) {
            Ok(roster) => Some(roster),
            Err(_) => {
                inner.lifecycle =
                    FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::PeerRevocation);
                return Ok(CommittedMutation {
                    value: None,
                    lifecycle: inner.lifecycle,
                });
            }
        };
        let lifecycle = reconcile_committed(&self.shared, &mut inner).await;
        Ok(CommittedMutation {
            value: roster,
            lifecycle,
        })
    }

    /// Commit one already live-authenticated address-only peer transition.
    /// Validation happens before worker reap; no core or journal mutation
    /// occurs when the proof is stale or names a different retained identity.
    pub(crate) async fn refresh_peer_address(
        &self,
        peer_id: DeviceId,
        expected_grant: &PeerGrant,
        expected_transport: &TransportBinding,
        candidate_transport: &TransportBinding,
        candidate_engine: &SyncEngineBinding,
    ) -> Result<CommittedMutation<()>, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        let before_revision = inner.journal.revision();
        let prepared = match inner.journal.prepare_peer_address_refresh(
            peer_id,
            expected_grant,
            expected_transport,
            candidate_transport,
            candidate_engine,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                if inner.journal.revision() != before_revision {
                    let _ = reconcile_committed(&self.shared, &mut inner).await;
                }
                return Err(map_journal_error(error));
            }
        };
        if prepared.is_already_complete() {
            return Ok(CommittedMutation {
                value: (),
                lifecycle: inner.lifecycle,
            });
        }
        quiesce(&mut inner, FolderSyncLifecycle::Stopped, false).await?;
        if let Err(error) = inner.journal.begin_peer_address_refresh(&prepared) {
            inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal);
            return Err(map_journal_error(error));
        }
        match self.shared.engine.refresh_trusted_peer_address(
            expected_grant,
            expected_transport,
            &candidate_transport.address,
        ) {
            Ok(_) => {}
            Err(_) => {
                inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal);
                return Err(FolderSyncServiceError::Journal);
            }
        }
        Ok(CommittedMutation {
            value: (),
            lifecycle: inner.lifecycle,
        })
    }

    /// Return a pending address transition only after checking whether core is
    /// still old or already contains the candidate. No journal state changes.
    pub(crate) async fn pending_peer_address_refresh(
        &self,
    ) -> Result<Option<(DeviceId, bool)>, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        inner
            .journal
            .pending_peer_address_refresh()
            .map_err(map_journal_error)
    }

    /// Confirm that an API retry names the exact pending/latest transition.
    pub(crate) async fn recognizes_peer_address_refresh(
        &self,
        peer_id: DeviceId,
        expected_address: &str,
        candidate_address: &str,
    ) -> Result<bool, FolderSyncServiceError> {
        let inner = self.try_inner()?;
        inner
            .journal
            .recognizes_peer_address_refresh(peer_id, expected_address, candidate_address)
            .map_err(map_journal_error)
    }

    /// Clear the final journal barrier only after AppState durably rebuilt any
    /// remembered provider route from current core trust.
    pub(crate) async fn finish_peer_address_refresh(
        &self,
        peer_id: DeviceId,
    ) -> Result<(), FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        inner
            .journal
            .complete_peer_address_refresh(peer_id)
            .map_err(|_| {
                inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal);
                FolderSyncServiceError::Journal
            })
    }

    /// Current public local engine route for an authenticated peer challenge.
    pub(crate) async fn current_binding(
        &self,
    ) -> Result<SyncEngineBinding, FolderSyncServiceError> {
        let inner = self.try_inner()?;
        inner
            .journal
            .authenticated_binding()
            .map_err(map_journal_error)
    }

    /// Return consent and the last observation from the owned health task.
    /// Polling must never perform worker I/O while holding the mutation lock:
    /// frequent UI requests would otherwise starve durable consent delivery.
    pub async fn status(&self) -> Result<FolderSyncStatus, FolderSyncServiceError> {
        let inner = self.try_inner()?;
        let shares = inner.journal.summaries().map_err(map_journal_error)?;
        Ok(FolderSyncStatus {
            lifecycle: inner.lifecycle,
            shares,
            folder_health: inner.folder_health.clone(),
            health_freshness: inner.health_freshness,
            peer_connections: inner.peer_connections.clone(),
            connection_freshness: inner.connection_freshness,
        })
    }

    async fn mutate<T>(
        &self,
        mutation: impl FnOnce(&mut FolderSharingJournal) -> Result<T, SharingError>,
    ) -> Result<CommittedMutation<T>, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        let before_revision = inner.journal.revision();
        let value = match mutation(&mut inner.journal) {
            Ok(value) => value,
            Err(error) => {
                let uncertain = matches!(
                    error,
                    SharingError::InvalidState
                        | SharingError::FolderUnavailable
                        | SharingError::PersistenceUncertain
                );
                if uncertain && inner.session.is_some() {
                    let _ = quiesce(
                        &mut inner,
                        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                        false,
                    )
                    .await;
                } else if uncertain {
                    inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal);
                } else if inner.journal.revision() != before_revision && inner.session.is_some() {
                    match inner.journal.desired_settings() {
                        Ok(desired) if inner.applied_settings.as_ref() == Some(&desired) => {}
                        Ok(_) => {
                            if quiesce(&mut inner, FolderSyncLifecycle::Stopped, true)
                                .await
                                .is_ok()
                                && let Err(issue) = start_desired(&self.shared, &mut inner).await
                            {
                                inner.lifecycle = FolderSyncLifecycle::NeedsAttention(issue);
                            }
                        }
                        Err(_) => {
                            let _ = quiesce(
                                &mut inner,
                                FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                                false,
                            )
                            .await;
                        }
                    }
                }
                return Err(map_journal_error(error));
            }
        };
        inner.automatic_runs_enabled = true;
        let lifecycle = reconcile_committed(&self.shared, &mut inner).await;
        Ok(CommittedMutation { value, lifecycle })
    }

    fn try_inner(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, ServiceInner>, FolderSyncServiceError> {
        self.shared
            .inner
            .try_lock()
            .map_err(|_| FolderSyncServiceError::Busy)
    }
}

impl Drop for FolderSyncService {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.health_task.take() {
            task.abort();
        }
        if let Ok(mut inner) = self.shared.inner.try_lock()
            && let Some(session) = &mut inner.session
        {
            session.close_lifeline();
        }
        // If a cancelled method or health probe temporarily owns the mutex,
        // dropping its last Arc drops ManagedEngineSession, whose Drop closes
        // the same exact lifeline. No process lookup or detached cleanup task is
        // introduced here.
    }
}

async fn reconcile_committed(
    shared: &ServiceShared,
    inner: &mut ServiceInner,
) -> FolderSyncLifecycle {
    let (desired, batches) = match inner
        .journal
        .desired_settings_and_runs(crate::now_unix_ms(), Instant::now())
    {
        Ok(desired) => desired,
        Err(_) => {
            if inner.session.is_some() {
                let _ = quiesce(
                    inner,
                    FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                    false,
                )
                .await;
            } else {
                inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal);
            }
            return inner.lifecycle;
        }
    };
    // ponytail: a new batch restarts the shared worker for a proven full scan.
    // Use per-folder scan generations if concurrent batches make this costly.
    let needs_batch_scan = batches
        .iter()
        .any(|(id, generation)| inner.applied_batch_generations.get(id) != Some(generation));
    inner
        .applied_batch_generations
        .retain(|id, generation| batches.get(id) == Some(generation));
    if !needs_batch_scan
        && matches!(
            inner.lifecycle,
            FolderSyncLifecycle::Running | FolderSyncLifecycle::InitialScanning
        )
        && inner.applied_settings.as_ref() == Some(&desired)
    {
        if inner.lifecycle == FolderSyncLifecycle::InitialScanning {
            advance_initial_scan(inner).await;
        }
        return inner.lifecycle;
    }
    if inner.session.is_some()
        && quiesce(inner, FolderSyncLifecycle::Stopped, true)
            .await
            .is_err()
    {
        return inner.lifecycle;
    }
    match start_desired(shared, inner).await {
        Ok(()) => inner.lifecycle,
        Err(issue) => {
            inner.lifecycle = FolderSyncLifecycle::NeedsAttention(issue);
            inner.lifecycle
        }
    }
}

async fn start_desired(
    shared: &ServiceShared,
    inner: &mut ServiceInner,
) -> Result<(), FolderSyncIssue> {
    let (settings, batches) = inner
        .journal
        .desired_settings_and_runs(crate::now_unix_ms(), Instant::now())
        .map_err(|_| FolderSyncIssue::Journal)?;
    apply_desired(shared, inner, settings, batches).await
}

async fn apply_desired(
    shared: &ServiceShared,
    inner: &mut ServiceInner,
    mut settings: EngineSessionSettings,
    mut batches: BTreeMap<Uuid, u64>,
) -> Result<(), FolderSyncIssue> {
    debug_assert!(inner.session.is_none());
    loop {
        if settings.folders.is_empty() {
            inner.applied_settings = None;
            inner.applied_batch_generations.clear();
            inner
                .folder_health
                .retain(|health| health.access_unavailable);
            inner.health_freshness = if inner.folder_health.is_empty() {
                FolderHealthFreshness::NeverObserved
            } else {
                FolderHealthFreshness::Stale
            };
            inner.peer_connections.clear();
            inner.connection_freshness = PeerConnectionFreshness::NeverObserved;
            inner.lifecycle = FolderSyncLifecycle::Stopped;
            return Ok(());
        }
        let pending_reset = inner
            .journal
            .pending_root_reset()
            .map_err(|_| FolderSyncIssue::Journal)?;
        let retained_settings = settings.clone();
        let retained_batches = batches.clone();
        inner.folder_health.clear();
        inner.health_freshness = FolderHealthFreshness::NeverObserved;
        inner.peer_connections.clear();
        inner.connection_freshness = PeerConnectionFreshness::NeverObserved;
        // Cancellation leaves a visible retryable problem while the reaper
        // releases the startup lease; it is not an ordinary stopped session.
        inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerLaunch);
        let mut session = shared
            .launcher
            .launch(settings, pending_reset.is_some())
            .await
            .map_err(|_| FolderSyncIssue::WorkerLaunch)?;
        if let Some(folder) = pending_reset {
            if session.reset_folder_index(folder).await.is_err() {
                inner.session = Some(session);
                let _ = quiesce(
                    inner,
                    FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::InitialScan),
                    false,
                )
                .await;
                return Err(FolderSyncIssue::InitialScan);
            }
            inner.session = Some(session);
            inner.applied_settings = Some(retained_settings);
            if quiesce(inner, FolderSyncLifecycle::Stopped, false)
                .await
                .is_err()
            {
                // The authenticated reset response is not completion. Keep
                // the durable intent and exact session handle until a later
                // retry confirms that this owned worker has been reaped.
                return Ok(());
            }
            inner
                .journal
                .complete_root_reset(folder)
                .map_err(|_| FolderSyncIssue::Journal)?;
            (settings, batches) = inner
                .journal
                .desired_settings_and_runs(crate::now_unix_ms(), Instant::now())
                .map_err(|_| FolderSyncIssue::Journal)?;
            continue;
        }
        let initial_scan = session.begin_initial_scan();
        inner.session = Some(session);
        inner.initial_scan = Some(initial_scan);
        inner.applied_settings = Some(retained_settings);
        inner.applied_batch_generations = retained_batches;
        inner.lifecycle = FolderSyncLifecycle::InitialScanning;
        tokio::task::yield_now().await;
        advance_initial_scan(inner).await;
        return if inner.lifecycle
            == FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::InitialScan)
        {
            Err(FolderSyncIssue::InitialScan)
        } else {
            Ok(())
        };
    }
}

async fn quiesce(
    inner: &mut ServiceInner,
    after_stop: FolderSyncLifecycle,
    restart_after_stop: bool,
) -> Result<(), FolderSyncServiceError> {
    let Some(session) = &mut inner.session else {
        inner.initial_scan.take();
        inner.applied_batch_generations.clear();
        inner.lifecycle = after_stop;
        inner.after_stop = after_stop;
        inner.restart_after_stop = false;
        return Ok(());
    };
    // This is deliberately synchronous and precedes the stop future.
    session.close_lifeline();
    if let Some(mut scan) = inner.initial_scan.take() {
        scan.abort();
        let _ = scan.finish().await;
    }
    if inner.health_freshness == FolderHealthFreshness::Fresh {
        inner.health_freshness = FolderHealthFreshness::Stale;
    }
    if inner.connection_freshness == PeerConnectionFreshness::Fresh {
        inner.connection_freshness = PeerConnectionFreshness::Stale;
    }
    inner.peer_connections.clear();
    inner.after_stop = after_stop;
    inner.restart_after_stop = restart_after_stop;
    inner.lifecycle = FolderSyncLifecycle::StillStopping;
    match session.stop().await {
        Ok(SessionStop::Exited) => {
            inner.session.take();
            inner.applied_settings = None;
            inner.applied_batch_generations.clear();
            inner.lifecycle = after_stop;
            inner.restart_after_stop = false;
            Ok(())
        }
        Ok(SessionStop::StillStopping) => {
            inner.lifecycle = FolderSyncLifecycle::StillStopping;
            Err(FolderSyncServiceError::WorkerStillStopping)
        }
        Err(()) => {
            inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerStop);
            Err(FolderSyncServiceError::WorkerStop)
        }
    }
}

async fn check_health(shared: &ServiceShared, inner: &mut ServiceInner) {
    if inner.lifecycle == FolderSyncLifecycle::StillStopping {
        let after_stop = inner.after_stop;
        let restart = inner.restart_after_stop;
        if quiesce(inner, after_stop, restart).await.is_ok()
            && restart
            && let Err(issue) = start_desired(shared, inner).await
        {
            inner.lifecycle = FolderSyncLifecycle::NeedsAttention(issue);
        }
        return;
    }
    if inner.lifecycle == FolderSyncLifecycle::InitialScanning {
        let desired = match inner.journal.desired_settings() {
            Ok(desired) => desired,
            Err(_) => {
                let _ = quiesce(
                    inner,
                    FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                    false,
                )
                .await;
                return;
            }
        };
        if inner.applied_settings.as_ref() != Some(&desired) {
            if quiesce(inner, FolderSyncLifecycle::Stopped, true)
                .await
                .is_ok()
                && let Err(issue) = start_desired(shared, inner).await
            {
                inner.lifecycle = FolderSyncLifecycle::NeedsAttention(issue);
            }
            return;
        }
        advance_initial_scan(inner).await;
        return;
    }
    if inner.lifecycle != FolderSyncLifecycle::Running {
        return;
    }
    let desired = match inner.journal.desired_settings() {
        Ok(desired) => desired,
        Err(_) => {
            let _ = quiesce(
                inner,
                FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                false,
            )
            .await;
            return;
        }
    };
    if inner.applied_settings.as_ref() != Some(&desired) {
        if quiesce(inner, FolderSyncLifecycle::Stopped, true)
            .await
            .is_ok()
            && let Err(issue) = start_desired(shared, inner).await
        {
            inner.lifecycle = FolderSyncLifecycle::NeedsAttention(issue);
        }
        return;
    }
    let (observation, connections) = match &mut inner.session {
        Some(session) => session.health().await,
        None => (Err(()), Err(())),
    };
    let current = match inner.journal.desired_settings() {
        Ok(current) => current,
        Err(_) => {
            let _ = quiesce(
                inner,
                FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                false,
            )
            .await;
            return;
        }
    };
    if inner.applied_settings.as_ref() != Some(&current) {
        if quiesce(inner, FolderSyncLifecycle::Stopped, true)
            .await
            .is_ok()
            && let Err(issue) = start_desired(shared, inner).await
        {
            inner.lifecycle = FolderSyncLifecycle::NeedsAttention(issue);
        }
        return;
    }
    match connections {
        Ok(connections) => match map_peer_connections(&inner.journal, connections) {
            Ok(connections) => {
                inner.peer_connections = connections;
                inner.connection_freshness = PeerConnectionFreshness::Fresh;
            }
            Err(()) => {
                inner.peer_connections.clear();
                inner.connection_freshness = stale_connection_freshness(inner.connection_freshness);
            }
        },
        Err(()) => {
            inner.peer_connections.clear();
            inner.connection_freshness = stale_connection_freshness(inner.connection_freshness);
        }
    }
    match observation {
        Ok(observation) => {
            let reported_error = observation.iter().any(FolderHealth::has_reported_errors);
            inner.folder_health = observation;
            inner.health_freshness = FolderHealthFreshness::Fresh;
            if !reported_error {
                return;
            }
        }
        Err(()) => {
            if inner.health_freshness == FolderHealthFreshness::Fresh {
                inner.health_freshness = FolderHealthFreshness::Stale;
            }
        }
    }
    let _ = quiesce(
        inner,
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerHealth),
        false,
    )
    .await;
}

fn stale_connection_freshness(current: PeerConnectionFreshness) -> PeerConnectionFreshness {
    if current == PeerConnectionFreshness::NeverObserved {
        PeerConnectionFreshness::NeverObserved
    } else {
        PeerConnectionFreshness::Stale
    }
}

fn map_peer_connections(
    journal: &FolderSharingJournal,
    connections: Vec<EnginePeerConnection>,
) -> Result<BTreeMap<DeviceId, PeerConnectionState>, ()> {
    let mut expected = journal.active_engine_peers().map_err(|_| ())?;
    if connections.len() != expected.len() {
        return Err(());
    }
    let mut result = BTreeMap::new();
    for connection in connections {
        let peer = expected.remove(connection.id()).ok_or(())?;
        let state = match connection.state() {
            EnginePeerConnectionState::Connected => PeerConnectionState::Connected,
            EnginePeerConnectionState::Disconnected => PeerConnectionState::Disconnected,
            EnginePeerConnectionState::Paused => PeerConnectionState::Paused,
        };
        if result.insert(peer, state).is_some() {
            return Err(());
        }
    }
    if !expected.is_empty() {
        return Err(());
    }
    Ok(result)
}

async fn advance_initial_scan(inner: &mut ServiceInner) {
    let Some(scan) = &inner.initial_scan else {
        let attention = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::InitialScan);
        if quiesce(inner, attention, false).await.is_err()
            && inner.lifecycle != FolderSyncLifecycle::StillStopping
        {
            inner.lifecycle = attention;
        }
        return;
    };
    if !scan.is_finished() {
        return;
    }
    let mut scan = inner.initial_scan.take().expect("scan checked above");
    let scan_result = scan.finish().await;
    let promotion_result = if scan_result.is_ok() {
        match inner
            .journal
            .desired_settings_and_runs(crate::now_unix_ms(), Instant::now())
        {
            Ok((settings, batches))
                if inner.applied_settings.as_ref() == Some(&settings)
                    && inner.applied_batch_generations == batches => {}
            Ok(_) => {
                let _ = quiesce(inner, FolderSyncLifecycle::Stopped, false).await;
                return;
            }
            Err(_) => {
                let _ = quiesce(
                    inner,
                    FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                    false,
                )
                .await;
                return;
            }
        }
        match &mut inner.session {
            Some(session) => session.promote_after_initial_scan().await,
            None => Err(()),
        }
    } else {
        Err(())
    };
    if promotion_result.is_ok() {
        inner.lifecycle = FolderSyncLifecycle::Running;
        return;
    }
    let attention = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::InitialScan);
    if quiesce(inner, attention, false).await.is_err()
        && inner.lifecycle != FolderSyncLifecycle::StillStopping
    {
        inner.lifecycle = attention;
    }
}

async fn advance_scheduled_runs(shared: &ServiceShared, inner: &mut ServiceInner, now: u64) {
    if !inner.automatic_runs_enabled {
        return;
    }
    let changes = (|| {
        let expired = inner.journal.expire_link_runs(now)?;
        let admitted = inner.journal.admit_due_link_runs(now)?;
        Ok::<_, SharingError>((expired, !admitted.is_empty()))
    })();
    match changes {
        Ok((expired, admitted)) => {
            if admitted
                || (expired
                    && matches!(
                        inner.lifecycle,
                        FolderSyncLifecycle::Running | FolderSyncLifecycle::InitialScanning
                    ))
            {
                reconcile_committed(shared, inner).await;
            }
        }
        Err(_) => {
            let _ = quiesce(
                inner,
                FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                false,
            )
            .await;
        }
    }
}

fn run_still_active(inner: &ServiceInner, folder: Uuid, generation: u64) -> bool {
    // Status requests may outlast a condition observation or run deadline.
    inner
        .journal
        .active_batch_generations(crate::now_unix_ms(), Instant::now())
        .is_ok_and(|active| active.get(&folder) == Some(&generation))
}

async fn advance_run_completion(shared: &ServiceShared, inner: &mut ServiceInner) {
    if inner.lifecycle != FolderSyncLifecycle::Running
        || inner.health_freshness != FolderHealthFreshness::Fresh
    {
        return;
    }
    let now = crate::now_unix_ms();
    let work = match inner.journal.link_run_work(now, Instant::now()) {
        Ok(work) => work,
        Err(_) => {
            let _ = quiesce(
                inner,
                FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                false,
            )
            .await;
            return;
        }
    };
    let mut changed = false;
    for item in work {
        let (folder_id, generation) = match &item {
            super::LinkRunWorkItem::PrepareSource {
                folder_id,
                generation,
                ..
            }
            | super::LinkRunWorkItem::ObserveDestination {
                folder_id,
                generation,
                ..
            } => (*folder_id, *generation),
        };
        if inner.applied_batch_generations.get(&folder_id) != Some(&generation)
            || !inner
                .folder_health
                .iter()
                .any(|health| health.folder == folder_id && !health.has_reported_errors())
        {
            continue;
        }
        let Some(session) = &mut inner.session else {
            return;
        };
        let expected = match &item {
            super::LinkRunWorkItem::PrepareSource { .. } => None,
            super::LinkRunWorkItem::ObserveDestination { source_index, .. } => Some(source_index),
        };
        let observed = match session.run_observation(folder_id, expected).await {
            Ok(observed) => observed,
            Err(RunObservationError::Retryable) => continue,
            Err(error @ RunObservationError::FolderUnavailable)
            | Err(error @ RunObservationError::PendingCopyRecoveryRequired) => {
                if !run_still_active(inner, folder_id, generation) {
                    continue;
                }
                let outcome = match item {
                    super::LinkRunWorkItem::PrepareSource { .. }
                        if error == RunObservationError::PendingCopyRecoveryRequired =>
                    {
                        continue;
                    }
                    super::LinkRunWorkItem::PrepareSource { .. } => inner
                        .journal
                        .interrupt_link_run(folder_id, generation, crate::now_unix_ms()),
                    super::LinkRunWorkItem::ObserveDestination { source_index, .. } => inner
                        .journal
                        .complete_link_run(
                            folder_id,
                            generation,
                            source_index,
                            super::LinkRunDestinationResult::Failed,
                            crate::now_unix_ms(),
                        )
                        .map(|_| ()),
                };
                match outcome {
                    Ok(()) => changed = true,
                    Err(SharingError::RunConflict) => {}
                    Err(_) => {
                        let _ = quiesce(
                            inner,
                            FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                            false,
                        )
                        .await;
                        return;
                    }
                }
                continue;
            }
        };
        let outcome = match item {
            super::LinkRunWorkItem::PrepareSource { .. } => {
                let Some(index) = observed.local_index else {
                    continue;
                };
                // Rclone derives the whole proof from one completed inventory.
                // Destinations compare their source inventory with this proof
                // before copying; a second local scan only repeats all hashes.
                if !run_still_active(inner, folder_id, generation) {
                    continue;
                }
                inner
                    .journal
                    .start_link_run(folder_id, generation, index, crate::now_unix_ms())
                    .map(|_| ())
            }
            super::LinkRunWorkItem::ObserveDestination {
                source_id: _,
                source_engine_id,
                source_index,
                ..
            } => {
                let Some(result) =
                    destination_observation_result(&observed, &source_engine_id, &source_index)
                else {
                    continue;
                };
                if !run_still_active(inner, folder_id, generation) {
                    continue;
                }
                inner
                    .journal
                    .complete_link_run(
                        folder_id,
                        generation,
                        source_index,
                        result,
                        crate::now_unix_ms(),
                    )
                    .map(|_| ())
            }
        };
        match outcome {
            Ok(()) => changed = true,
            Err(SharingError::RunConflict) => {}
            Err(_) => {
                let _ = quiesce(
                    inner,
                    FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal),
                    false,
                )
                .await;
                return;
            }
        }
    }
    if changed {
        reconcile_committed(shared, inner).await;
    }
}

pub(super) fn destination_observation_result(
    observed: &super::run_observation::EngineRunObservation,
    source: &super::config::EngineDeviceId,
    expected: &super::EngineIndexSnapshot,
) -> Option<super::LinkRunDestinationResult> {
    if observed
        .completions
        .get(source)
        .is_some_and(|index| index.reaches(expected))
    {
        Some(super::LinkRunDestinationResult::Succeeded)
    } else if observed.failures.contains(source) {
        Some(super::LinkRunDestinationResult::Failed)
    } else {
        None
    }
}

async fn health_loop(
    shared: Weak<ServiceShared>,
    mut shutdown: watch::Receiver<bool>,
    cadence: Duration,
) {
    // Leave a full cadence after each completed probe. A slow worker must not
    // trigger catch-up probes that continuously occupy the mutation lock.
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(cadence) => {
                let Some(shared) = shared.upgrade() else {
                    break;
                };
                let mut inner = shared.inner.lock().await;
                advance_scheduled_runs(&shared, &mut inner, crate::now_unix_ms()).await;
                check_health(&shared, &mut inner).await;
                advance_run_completion(&shared, &mut inner).await;
            }
        }
    }
}

fn map_journal_error(error: SharingError) -> FolderSyncServiceError {
    match error {
        SharingError::SettingsConflict => FolderSyncServiceError::SettingsConflict,
        SharingError::SettingsPending => FolderSyncServiceError::SettingsPending,
        SharingError::RunConflict => FolderSyncServiceError::RunConflict,
        SharingError::RunPending => FolderSyncServiceError::RunPending,
        _ => FolderSyncServiceError::Journal,
    }
}

fn map_start_issue(issue: FolderSyncIssue) -> FolderSyncServiceError {
    if issue == FolderSyncIssue::Journal {
        FolderSyncServiceError::Journal
    } else {
        FolderSyncServiceError::WorkerLaunch
    }
}

#[cfg(test)]
use std::collections::{BTreeSet, VecDeque};
#[cfg(test)]
use std::sync::Mutex as StdMutex;
#[cfg(test)]
use tokio::sync::Notify;

#[cfg(test)]
#[derive(Clone)]
pub(super) enum TestStopBehavior {
    Exited,
    StillStopping,
    Fail,
    Wait(Arc<Notify>),
}

#[cfg(test)]
#[derive(Clone)]
pub(super) enum TestScanBehavior {
    Complete,
    Fail,
    Wait(Arc<Notify>),
}

#[cfg(test)]
#[derive(Default)]
struct TestBackendState {
    run_observations: BTreeMap<Uuid, VecDeque<super::run_observation::EngineRunObservation>>,
    unavailable_run_folders: BTreeSet<Uuid>,
    pending_copy_recovery_folders: BTreeSet<Uuid>,
    fail_launches: usize,
    fail_resets: usize,
    fail_promotions: usize,
    fail_health: bool,
    health_observation: Vec<FolderHealth>,
    fail_connections: bool,
    connection_states: Vec<EnginePeerConnectionState>,
    launches: usize,
    active: usize,
    maximum_active: usize,
    close_calls: usize,
    stop_calls: usize,
    health_calls: usize,
    connection_calls: usize,
    scan_calls: usize,
    reset_calls: Vec<Uuid>,
    promotions: usize,
    launched_folder_counts: Vec<usize>,
    launched_reset_gates: Vec<bool>,
    stops: VecDeque<TestStopBehavior>,
    scans: VecDeque<TestScanBehavior>,
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct TestBackend {
    state: StdMutex<TestBackendState>,
    health_observed: Notify,
    stop_observed: Notify,
}

#[cfg(test)]
impl TestBackend {
    pub(super) fn push_stop(&self, behavior: TestStopBehavior) {
        self.state.lock().unwrap().stops.push_back(behavior);
    }

    pub(super) fn fail_next_launch(&self) {
        self.state.lock().unwrap().fail_launches += 1;
    }

    pub(super) fn fail_next_reset(&self) {
        self.state.lock().unwrap().fail_resets += 1;
    }

    pub(super) fn push_scan(&self, behavior: TestScanBehavior) {
        self.state.lock().unwrap().scans.push_back(behavior);
    }

    pub(super) fn fail_next_promotion(&self) {
        self.state.lock().unwrap().fail_promotions += 1;
    }

    pub(super) fn set_health_failure(&self, fail: bool) {
        self.state.lock().unwrap().fail_health = fail;
    }

    pub(super) fn set_run_observations(
        &self,
        folder: Uuid,
        observations: Vec<super::run_observation::EngineRunObservation>,
    ) {
        self.state
            .lock()
            .unwrap()
            .run_observations
            .insert(folder, observations.into());
    }

    pub(super) fn set_run_folder_unavailable(&self, folder: Uuid) {
        self.state
            .lock()
            .unwrap()
            .unavailable_run_folders
            .insert(folder);
    }

    pub(super) fn set_pending_copy_recovery_required(&self, folder: Uuid) {
        self.state
            .lock()
            .unwrap()
            .pending_copy_recovery_folders
            .insert(folder);
    }

    pub(super) fn set_health_observation(&self, observation: Vec<FolderHealth>) {
        self.state.lock().unwrap().health_observation = observation;
    }

    pub(super) fn set_connection_failure(&self, fail: bool) {
        self.state.lock().unwrap().fail_connections = fail;
    }

    pub(super) fn set_connection_states(&self, states: Vec<EnginePeerConnectionState>) {
        self.state.lock().unwrap().connection_states = states;
    }

    pub(super) fn snapshot(&self) -> TestBackendSnapshot {
        let state = self.state.lock().unwrap();
        TestBackendSnapshot {
            launches: state.launches,
            active: state.active,
            maximum_active: state.maximum_active,
            close_calls: state.close_calls,
            stop_calls: state.stop_calls,
            health_calls: state.health_calls,
            connection_calls: state.connection_calls,
            scan_calls: state.scan_calls,
            reset_calls: state.reset_calls.clone(),
            promotions: state.promotions,
            launched_folder_counts: state.launched_folder_counts.clone(),
            launched_reset_gates: state.launched_reset_gates.clone(),
        }
    }

    pub(super) async fn wait_for_health(&self) {
        self.health_observed.notified().await;
    }

    pub(super) async fn wait_for_stop(&self) {
        self.stop_observed.notified().await;
    }

    async fn launch(
        self: &Arc<Self>,
        settings: EngineSessionSettings,
        reset_gate: bool,
    ) -> Result<TestSession, EngineSessionError> {
        let mut state = self.state.lock().unwrap();
        if state.fail_launches > 0 {
            state.fail_launches -= 1;
            return Err(EngineSessionError::LaunchFailed);
        }
        state.launches += 1;
        state.active += 1;
        state.maximum_active = state.maximum_active.max(state.active);
        state.launched_folder_counts.push(settings.folders.len());
        state.launched_reset_gates.push(reset_gate);
        Ok(TestSession {
            backend: Arc::clone(self),
            closed: false,
            peer_ids: settings
                .peers
                .iter()
                .map(|peer| peer.id().clone())
                .collect(),
        })
    }
}

#[cfg(test)]
pub(super) struct TestBackendSnapshot {
    pub(super) launches: usize,
    pub(super) active: usize,
    pub(super) maximum_active: usize,
    pub(super) close_calls: usize,
    pub(super) stop_calls: usize,
    pub(super) health_calls: usize,
    pub(super) connection_calls: usize,
    pub(super) scan_calls: usize,
    pub(super) reset_calls: Vec<Uuid>,
    pub(super) promotions: usize,
    pub(super) launched_folder_counts: Vec<usize>,
    pub(super) launched_reset_gates: Vec<bool>,
}

#[cfg(test)]
struct TestSession {
    backend: Arc<TestBackend>,
    closed: bool,
    peer_ids: Vec<super::config::EngineDeviceId>,
}

#[cfg(test)]
impl TestSession {
    fn run_observation(
        &self,
        folder: Uuid,
    ) -> Result<super::run_observation::EngineRunObservation, RunObservationError> {
        let mut state = self.backend.state.lock().unwrap();
        if state.unavailable_run_folders.contains(&folder) {
            return Err(RunObservationError::FolderUnavailable);
        }
        if state.pending_copy_recovery_folders.contains(&folder) {
            return Err(RunObservationError::PendingCopyRecoveryRequired);
        }
        let observations = state
            .run_observations
            .get_mut(&folder)
            .ok_or(RunObservationError::Retryable)?;
        if observations.len() > 1 {
            observations
                .pop_front()
                .ok_or(RunObservationError::Retryable)
        } else {
            observations
                .front()
                .cloned()
                .ok_or(RunObservationError::Retryable)
        }
    }

    async fn reset_folder_index(&mut self, folder: Uuid) -> Result<(), ()> {
        let mut state = self.backend.state.lock().unwrap();
        state.reset_calls.push(folder);
        if state.fail_resets > 0 {
            state.fail_resets -= 1;
            Err(())
        } else {
            Ok(())
        }
    }

    fn begin_initial_scan(&self) -> InitialScanTask {
        let behavior = {
            let mut state = self.backend.state.lock().unwrap();
            state.scan_calls += 1;
            state
                .scans
                .pop_front()
                .unwrap_or(TestScanBehavior::Complete)
        };
        InitialScanTask::new(tokio::spawn(async move {
            match behavior {
                TestScanBehavior::Complete => Ok(()),
                TestScanBehavior::Fail => Err(EngineSessionError::EngineUnavailable),
                TestScanBehavior::Wait(notify) => {
                    notify.notified().await;
                    Ok(())
                }
            }
        }))
    }

    async fn promote_after_initial_scan(&mut self) -> Result<(), ()> {
        let fail = {
            let mut state = self.backend.state.lock().unwrap();
            state.promotions += 1;
            let fail = state.fail_promotions > 0;
            state.fail_promotions = state.fail_promotions.saturating_sub(1);
            fail
        };
        if fail {
            self.close_lifeline();
            Err(())
        } else {
            Ok(())
        }
    }

    fn close_lifeline(&mut self) {
        if !self.closed {
            self.closed = true;
            self.backend.state.lock().unwrap().close_calls += 1;
        }
    }

    async fn stop(&mut self) -> Result<SessionStop, ()> {
        let behavior = {
            let mut state = self.backend.state.lock().unwrap();
            state.stop_calls += 1;
            assert!(self.closed, "the lifeline must close before stop awaits");
            state.stops.pop_front().unwrap_or(TestStopBehavior::Exited)
        };
        self.backend.stop_observed.notify_one();
        match behavior {
            TestStopBehavior::Exited => Ok(SessionStop::Exited),
            TestStopBehavior::StillStopping => Ok(SessionStop::StillStopping),
            TestStopBehavior::Fail => Err(()),
            TestStopBehavior::Wait(notify) => {
                notify.notified().await;
                Ok(SessionStop::Exited)
            }
        }
    }

    async fn health(
        &mut self,
    ) -> (
        Result<Vec<FolderHealth>, ()>,
        Result<Vec<EnginePeerConnection>, ()>,
    ) {
        let mut state = self.backend.state.lock().unwrap();
        state.health_calls += 1;
        state.connection_calls += 1;
        self.backend.health_observed.notify_one();
        let folders = if state.fail_health {
            Err(())
        } else {
            Ok(state.health_observation.clone())
        };
        let connections = if state.fail_connections {
            Err(())
        } else {
            let states = if state.connection_states.is_empty() {
                vec![EnginePeerConnectionState::Disconnected; self.peer_ids.len()]
            } else {
                state.connection_states.clone()
            };
            if states.len() != self.peer_ids.len() {
                Err(())
            } else {
                Ok(self
                    .peer_ids
                    .iter()
                    .cloned()
                    .zip(states)
                    .map(|(id, state)| EnginePeerConnection::new(id, state))
                    .collect())
            }
        };
        (folders, connections)
    }
}

#[cfg(test)]
impl Drop for TestSession {
    fn drop(&mut self) {
        self.close_lifeline();
        self.backend.state.lock().unwrap().active -= 1;
    }
}

#[cfg(test)]
impl FolderSyncService {
    pub(super) fn new_for_test(
        journal: FolderSharingJournal,
        engine: Arc<Engine>,
        backend: Arc<TestBackend>,
        health_interval: Duration,
    ) -> Self {
        Self::construct(journal, engine, Launcher::Test(backend), health_interval).unwrap()
    }

    pub(super) async fn expire_android_conditions_for_test(&self) {
        self.shared
            .inner
            .lock()
            .await
            .journal
            .observe_android_conditions(true, true, Instant::now() - Duration::from_secs(91));
    }

    pub(super) async fn advance_runs_for_test(&self) {
        let mut inner = self.shared.inner.lock().await;
        advance_scheduled_runs(&self.shared, &mut inner, crate::now_unix_ms()).await;
        check_health(&self.shared, &mut inner).await;
        advance_run_completion(&self.shared, &mut inner).await;
    }

    pub(super) async fn observe_health_for_test(&self) {
        let mut inner = self.shared.inner.lock().await;
        check_health(&self.shared, &mut inner).await;
    }

    pub(super) fn cached_lifecycle_for_test(&self) -> FolderSyncLifecycle {
        self.shared.inner.try_lock().unwrap().lifecycle
    }
}
