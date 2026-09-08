//! Serialized durable consent and owned worker lifecycle.
//!
//! This service is the only boundary that combines folder-sharing journal
//! mutations with worker replacement. A signed decision is returned only after
//! the journal made it durable. Worker failure after that point is reported in
//! the returned lifecycle and never rolls the decision back.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use covalent_core::Engine;
use covalent_protocol::{
    DeviceId, FolderShareAcceptance, FolderShareCommit, FolderShareOffer, SignedRoster,
};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{
    EngineInstallation, EngineSessionError, EngineSessionSettings, FolderHealth,
    FolderSharingJournal, ManagedEngineSession, ShareSummary, SharingError, StopOutcome,
    VerifiedEngineExecutable,
};

const HEALTH_INTERVAL: Duration = Duration::from_secs(5);

/// Stable reason that a durable service needs an explicit retry or repair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderSyncIssue {
    Journal,
    WorkerLaunch,
    WorkerHealth,
    WorkerStop,
    PeerRevocation,
}

/// Current worker lifecycle. This does not claim that folders are up to date.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderSyncLifecycle {
    Stopped,
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
    async fn launch(&self, settings: EngineSessionSettings) -> Result<Session, EngineSessionError> {
        match self {
            Self::Production(launcher) => ManagedEngineSession::start(
                Arc::clone(&launcher.installation),
                &launcher.guardian,
                &launcher.engine,
                &launcher.runtime_parent,
                settings,
            )
            .await
            .map(Box::new)
            .map(Session::Production),
            #[cfg(test)]
            Self::Test(backend) => backend.launch(settings).await.map(Session::Test),
        }
    }
}

impl Session {
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

    async fn health(&mut self) -> Result<Vec<FolderHealth>, ()> {
        match self {
            Self::Production(session) => session.folder_health().await.map_err(|_| ()),
            #[cfg(test)]
            Self::Test(session) => session.health().await,
        }
    }
}

struct ServiceInner {
    journal: FolderSharingJournal,
    session: Option<Session>,
    applied_settings: Option<EngineSessionSettings>,
    lifecycle: FolderSyncLifecycle,
    after_stop: FolderSyncLifecycle,
    restart_after_stop: bool,
    folder_health: Vec<FolderHealth>,
    health_freshness: FolderHealthFreshness,
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
        journal: FolderSharingJournal,
        engine: Arc<Engine>,
        launcher: Launcher,
        health_interval: Duration,
    ) -> Result<Self, FolderSyncServiceError> {
        if health_interval.is_zero() {
            return Err(FolderSyncServiceError::InvalidConfiguration);
        }
        let (shutdown, receiver) = watch::channel(false);
        let shared = Arc::new(ServiceShared {
            inner: Mutex::new(ServiceInner {
                journal,
                session: None,
                applied_settings: None,
                lifecycle: FolderSyncLifecycle::Stopped,
                after_stop: FolderSyncLifecycle::Stopped,
                restart_after_stop: false,
                folder_health: Vec::new(),
                health_freshness: FolderHealthFreshness::NeverObserved,
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
        let desired = match inner.journal.desired_settings() {
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
        if inner.lifecycle == FolderSyncLifecycle::Running
            && inner.applied_settings.as_ref() == Some(&desired)
        {
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
        if let Err(issue) = apply_desired(&self.shared, &mut inner, desired).await {
            inner.lifecycle = FolderSyncLifecycle::NeedsAttention(issue);
            return Err(FolderSyncServiceError::WorkerLaunch);
        }
        Ok(inner.lifecycle)
    }

    /// Stop and reap the exact owned worker. `StillStopping` remains explicit.
    pub async fn stop(&self) -> Result<FolderSyncLifecycle, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
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
        self.mutate(|journal| journal.offer(peer_id, folder_id, label, selected_root, now))
            .await
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
        if inner.lifecycle == FolderSyncLifecycle::Running {
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

    pub async fn accept(
        &self,
        offer_id: Uuid,
        selected_root: &Path,
        now: u64,
    ) -> Result<CommittedMutation<FolderShareAcceptance>, FolderSyncServiceError> {
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

    /// Revalidate worker health and return a secret-free consent snapshot.
    pub async fn status(&self) -> Result<FolderSyncStatus, FolderSyncServiceError> {
        let mut inner = self.try_inner()?;
        check_health(&self.shared, &mut inner).await;
        let shares = inner.journal.summaries().map_err(map_journal_error)?;
        Ok(FolderSyncStatus {
            lifecycle: inner.lifecycle,
            shares,
            folder_health: inner.folder_health.clone(),
            health_freshness: inner.health_freshness,
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
    let desired = match inner.journal.desired_settings() {
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
    if inner.lifecycle == FolderSyncLifecycle::Running
        && inner.applied_settings.as_ref() == Some(&desired)
    {
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
    let settings = inner
        .journal
        .desired_settings()
        .map_err(|_| FolderSyncIssue::Journal)?;
    apply_desired(shared, inner, settings).await
}

async fn apply_desired(
    shared: &ServiceShared,
    inner: &mut ServiceInner,
    settings: EngineSessionSettings,
) -> Result<(), FolderSyncIssue> {
    debug_assert!(inner.session.is_none());
    if settings.folders.is_empty() {
        inner.applied_settings = None;
        inner.folder_health.clear();
        inner.health_freshness = FolderHealthFreshness::NeverObserved;
        inner.lifecycle = FolderSyncLifecycle::Stopped;
        return Ok(());
    }
    let retained_settings = settings.clone();
    inner.folder_health.clear();
    inner.health_freshness = FolderHealthFreshness::NeverObserved;
    // Cancellation leaves a visible retryable problem while the reaper
    // releases the startup lease; it is not an ordinary stopped session.
    inner.lifecycle = FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerLaunch);
    match shared.launcher.launch(settings).await {
        Ok(session) => {
            inner.session = Some(session);
            inner.applied_settings = Some(retained_settings);
            inner.lifecycle = FolderSyncLifecycle::Running;
            Ok(())
        }
        Err(_) => Err(FolderSyncIssue::WorkerLaunch),
    }
}

async fn quiesce(
    inner: &mut ServiceInner,
    after_stop: FolderSyncLifecycle,
    restart_after_stop: bool,
) -> Result<(), FolderSyncServiceError> {
    let Some(session) = &mut inner.session else {
        inner.lifecycle = after_stop;
        inner.after_stop = after_stop;
        inner.restart_after_stop = false;
        return Ok(());
    };
    // This is deliberately synchronous and precedes the stop future.
    session.close_lifeline();
    if inner.health_freshness == FolderHealthFreshness::Fresh {
        inner.health_freshness = FolderHealthFreshness::Stale;
    }
    inner.after_stop = after_stop;
    inner.restart_after_stop = restart_after_stop;
    inner.lifecycle = FolderSyncLifecycle::StillStopping;
    match session.stop().await {
        Ok(SessionStop::Exited) => {
            inner.session.take();
            inner.applied_settings = None;
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
    let observation = match &mut inner.session {
        Some(session) => session.health().await,
        None => Err(()),
    };
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

async fn health_loop(
    shared: Weak<ServiceShared>,
    mut shutdown: watch::Receiver<bool>,
    cadence: Duration,
) {
    let mut interval = tokio::time::interval(cadence);
    // Startup performs its own complete verification. Avoid an immediate
    // duplicate API request from interval's first ready tick.
    interval.tick().await;
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = interval.tick() => {
                let Some(shared) = shared.upgrade() else {
                    break;
                };
                let mut inner = shared.inner.lock().await;
                check_health(&shared, &mut inner).await;
            }
        }
    }
}

fn map_journal_error(_: SharingError) -> FolderSyncServiceError {
    FolderSyncServiceError::Journal
}

fn map_start_issue(issue: FolderSyncIssue) -> FolderSyncServiceError {
    if issue == FolderSyncIssue::Journal {
        FolderSyncServiceError::Journal
    } else {
        FolderSyncServiceError::WorkerLaunch
    }
}

#[cfg(test)]
use std::collections::VecDeque;
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
#[derive(Default)]
struct TestBackendState {
    fail_launches: usize,
    fail_health: bool,
    health_observation: Vec<FolderHealth>,
    launches: usize,
    active: usize,
    maximum_active: usize,
    close_calls: usize,
    stop_calls: usize,
    health_calls: usize,
    launched_folder_counts: Vec<usize>,
    stops: VecDeque<TestStopBehavior>,
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

    pub(super) fn set_health_failure(&self, fail: bool) {
        self.state.lock().unwrap().fail_health = fail;
    }

    pub(super) fn set_health_observation(&self, observation: Vec<FolderHealth>) {
        self.state.lock().unwrap().health_observation = observation;
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
            launched_folder_counts: state.launched_folder_counts.clone(),
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
        Ok(TestSession {
            backend: Arc::clone(self),
            closed: false,
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
    pub(super) launched_folder_counts: Vec<usize>,
}

#[cfg(test)]
struct TestSession {
    backend: Arc<TestBackend>,
    closed: bool,
}

#[cfg(test)]
impl TestSession {
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

    async fn health(&mut self) -> Result<Vec<FolderHealth>, ()> {
        let mut state = self.backend.state.lock().unwrap();
        state.health_calls += 1;
        self.backend.health_observed.notify_one();
        if state.fail_health {
            Err(())
        } else {
            Ok(state.health_observation.clone())
        }
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

    pub(super) fn cached_lifecycle_for_test(&self) -> FolderSyncLifecycle {
        self.shared.inner.try_lock().unwrap().lifecycle
    }
}
