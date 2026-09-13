//! Bounded source-authoritative state for manual and scheduled link runs.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::*;
use crate::sync_engine::EngineIndexSnapshot;

pub const LINK_RUN_DEADLINE_MS: u64 = 24 * 60 * 60 * 1_000;
const ANDROID_CONDITION_TTL: Duration = Duration::from_secs(90);
const CONTINUOUS_CHECK_INTERVAL_MS: u64 = 15_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkRunPhase {
    Preparing,
    Running,
    Succeeded,
    Incomplete,
    Interrupted,
    Cancelled,
}

impl LinkRunPhase {
    fn active(self) -> bool {
        matches!(self, Self::Preparing | Self::Running)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkRunDestinationResult {
    Pending,
    Succeeded,
    Failed,
    TimedOut,
    Interrupted,
    Cancelled,
}

impl LinkRunDestinationResult {
    fn terminal(self) -> bool {
        self != Self::Pending
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRunDestinationState {
    pub result: LinkRunDestinationResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRunState {
    pub generation: u64,
    pub state_revision: u64,
    pub settings_revision: u64,
    pub request_id: Uuid,
    pub requested_by: DeviceId,
    pub phase: LinkRunPhase,
    pub started_at_unix_ms: u64,
    pub deadline_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_index: Option<EngineIndexSnapshot>,
    pub destinations: BTreeMap<DeviceId, LinkRunDestinationState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRunRequest {
    pub folder_id: Uuid,
    pub source_id: DeviceId,
    pub requester_id: DeviceId,
    pub request_id: Uuid,
    pub expected_generation: u64,
    pub settings_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkRunRejectionReason {
    GenerationChanged,
    SettingsChanged,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRunRejection {
    pub requester_id: DeviceId,
    pub request_id: Uuid,
    pub reason: LinkRunRejectionReason,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRunCommit {
    pub folder_id: Uuid,
    pub source_id: DeviceId,
    pub target_id: DeviceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<LinkRunState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_request_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_request: Option<LinkRunRejection>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRunReport {
    pub folder_id: Uuid,
    pub source_id: DeviceId,
    pub reporter_id: DeviceId,
    pub generation: u64,
    pub settings_revision: u64,
    pub source_index: EngineIndexSnapshot,
    pub result: LinkRunDestinationResult,
    pub ended_at_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkRunAdmission {
    Accepted(LinkRunState),
    Pending(LinkRunRequest),
    AlreadyRunning(LinkRunState),
    Rejected(LinkRunRejection),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkRunWorkItem {
    PrepareSource {
        folder_id: Uuid,
        generation: u64,
        settings_revision: u64,
    },
    ObserveDestination {
        folder_id: Uuid,
        generation: u64,
        settings_revision: u64,
        source_id: DeviceId,
        source_engine_id: EngineDeviceId,
        source_index: EngineIndexSnapshot,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRunDestinationSummary {
    pub peer_id: DeviceId,
    pub result: LinkRunDestinationResult,
    pub ended_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRunRequestSummary {
    pub request_id: Uuid,
    pub requester_id: DeviceId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRunSummary {
    pub generation: u64,
    pub state_revision: u64,
    pub settings_revision: u64,
    pub phase: Option<LinkRunPhase>,
    pub started_at_unix_ms: Option<u64>,
    pub deadline_unix_ms: Option<u64>,
    pub ended_at_unix_ms: Option<u64>,
    pub next_due_at_unix_ms: Option<u64>,
    pub pending_request: Option<LinkRunRequestSummary>,
    pub rejected_request: Option<LinkRunRejection>,
    pub destinations: Vec<LinkRunDestinationSummary>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LinkRunLinkState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authoritative: Option<LinkRunState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_request: Option<LinkRunRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rejected_request: Option<LinkRunRejection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_report: Option<LinkRunReport>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct AndroidConditionObservation {
    wifi_connected: bool,
    charging: bool,
    observed_at: Instant,
}

impl FolderSharingJournal {
    pub fn set_android_host(&mut self, is_android: bool) {
        self.android_host = is_android;
        if !is_android {
            self.android_condition_observation = None;
        }
    }

    pub fn observe_android_conditions(
        &mut self,
        wifi_connected: bool,
        charging: bool,
        observed_at: Instant,
    ) {
        self.android_condition_observation = Some(AndroidConditionObservation {
            wifi_connected,
            charging,
            observed_at,
        });
    }

    pub fn request_link_run(
        &mut self,
        folder_id: Uuid,
        request_id: Uuid,
        expected_generation: u64,
        settings_revision: u64,
        now_unix_ms: u64,
    ) -> Result<LinkRunAdmission, SharingError> {
        self.reconcile_trust()?;
        if request_id.is_nil() || now_unix_ms == 0 {
            return Err(SharingError::InvalidRecord);
        }
        let source_id = self.link_source(folder_id)?;
        let request = LinkRunRequest {
            folder_id,
            source_id,
            requester_id: self.engine.device_id(),
            request_id,
            expected_generation,
            settings_revision,
        };
        if source_id == self.engine.device_id() {
            let mut next = self.snapshot.clone();
            let (admission, changed) =
                admit_source_request(&mut next, &request, now_unix_ms, false)?;
            if changed {
                self.persist(next)?;
            }
            return Ok(admission);
        }
        validate_request_membership(&self.snapshot, &request)?;
        let settings = self
            .snapshot
            .link_settings
            .get(&folder_id)
            .ok_or(SharingError::InvalidState)?;
        if !settings.confirmed || settings.revision != settings_revision {
            return Err(SharingError::SettingsConflict);
        }
        if let Some(current) = self
            .snapshot
            .link_runs
            .get(&folder_id)
            .and_then(|slot| slot.authoritative.as_ref())
            && current.request_id == request_id
            && current.requested_by == self.engine.device_id()
            && current.settings_revision == settings_revision
            && expected_generation.checked_add(1) == Some(current.generation)
        {
            return Ok(LinkRunAdmission::AlreadyRunning(current.clone()));
        }
        let current_generation = current_generation(&self.snapshot, folder_id);
        if current_generation != expected_generation {
            return Err(SharingError::RunConflict);
        }
        let mut next = self.snapshot.clone();
        let slot = next.link_runs.entry(folder_id).or_default();
        if let Some(pending) = &slot.pending_request {
            return if pending == &request {
                Ok(LinkRunAdmission::Pending(request))
            } else {
                Err(SharingError::RunPending)
            };
        }
        slot.pending_request = Some(request.clone());
        slot.rejected_request = None;
        self.persist(next)?;
        Ok(LinkRunAdmission::Pending(request))
    }

    /// The caller authenticated requester_id through the link-control envelope.
    pub fn receive_link_run_request(
        &mut self,
        request: &LinkRunRequest,
        now_unix_ms: u64,
    ) -> Result<LinkRunCommit, SharingError> {
        self.reconcile_trust()?;
        if request.source_id != self.engine.device_id() || request.request_id.is_nil() {
            return Err(SharingError::InvalidRecord);
        }
        validate_request_membership(&self.snapshot, request)?;
        let mut next = self.snapshot.clone();
        let (_, changed) = admit_source_request(&mut next, request, now_unix_ms, false)?;
        let mut commit = commit_for_target(&next, request.folder_id, request.requester_id)?;
        commit.acknowledged_request_id = Some(request.request_id);
        if changed {
            self.persist(next)?;
        }
        Ok(commit)
    }

    pub fn start_link_run(
        &mut self,
        folder_id: Uuid,
        generation: u64,
        source_index: EngineIndexSnapshot,
        now_unix_ms: u64,
    ) -> Result<LinkRunState, SharingError> {
        if !source_index.is_valid() || now_unix_ms == 0 {
            return Err(SharingError::InvalidRecord);
        }
        if self.link_source(folder_id)? != self.engine.device_id() {
            return Err(SharingError::InvalidRecord);
        }
        let mut next = self.snapshot.clone();
        let state = next
            .link_runs
            .get_mut(&folder_id)
            .and_then(|slot| slot.authoritative.as_mut())
            .ok_or(SharingError::InvalidState)?;
        if state.generation != generation || state.phase != LinkRunPhase::Preparing {
            return Err(SharingError::RunConflict);
        }
        if now_unix_ms < state.started_at_unix_ms || now_unix_ms >= state.deadline_unix_ms {
            return Err(SharingError::RunConflict);
        }
        state.phase = LinkRunPhase::Running;
        state.state_revision = next_state_revision(state.state_revision)?;
        state.source_index = Some(source_index);
        let result = state.clone();
        self.persist(next)?;
        Ok(result)
    }

    pub fn complete_link_run(
        &mut self,
        folder_id: Uuid,
        generation: u64,
        source_index: EngineIndexSnapshot,
        result: LinkRunDestinationResult,
        now_unix_ms: u64,
    ) -> Result<LinkRunReport, SharingError> {
        if !source_index.is_valid()
            || !matches!(
                result,
                LinkRunDestinationResult::Succeeded | LinkRunDestinationResult::Failed
            )
            || now_unix_ms == 0
        {
            return Err(SharingError::InvalidRecord);
        }
        let source_id = self.link_source(folder_id)?;
        if source_id == self.engine.device_id() {
            return Err(SharingError::InvalidRecord);
        }
        let current = self
            .snapshot
            .link_runs
            .get(&folder_id)
            .and_then(|slot| slot.authoritative.as_ref())
            .ok_or(SharingError::InvalidState)?;
        if current.generation != generation
            || current.phase != LinkRunPhase::Running
            || current.source_index.as_ref() != Some(&source_index)
            || !current.destinations.contains_key(&self.engine.device_id())
            || now_unix_ms < current.started_at_unix_ms
            || now_unix_ms >= current.deadline_unix_ms
        {
            return Err(SharingError::RunConflict);
        }
        let report = LinkRunReport {
            folder_id,
            source_id,
            reporter_id: self.engine.device_id(),
            generation,
            settings_revision: current.settings_revision,
            source_index,
            result,
            ended_at_unix_ms: now_unix_ms,
        };
        let mut next = self.snapshot.clone();
        let slot = next
            .link_runs
            .get_mut(&folder_id)
            .ok_or(SharingError::InvalidState)?;
        if let Some(pending) = &slot.pending_report {
            return if pending == &report {
                Ok(report)
            } else {
                Err(SharingError::RunConflict)
            };
        }
        slot.pending_report = Some(report.clone());
        self.persist(next)?;
        Ok(report)
    }

    /// The caller authenticated source_id through the link-control envelope.
    pub fn receive_link_run_commit(&mut self, commit: &LinkRunCommit) -> Result<(), SharingError> {
        self.reconcile_trust()?;
        validate_commit_for_local(&self.snapshot, self.engine.device_id(), commit)?;
        let current = self
            .snapshot
            .link_runs
            .get(&commit.folder_id)
            .and_then(|slot| slot.authoritative.as_ref());
        if let (Some(current), Some(incoming)) = (current, commit.state.as_ref()) {
            if incoming.generation < current.generation
                || (incoming.generation == current.generation
                    && incoming.state_revision < current.state_revision)
            {
                return Ok(());
            }
            if incoming.generation == current.generation
                && incoming.state_revision == current.state_revision
                && current != incoming
            {
                return Err(SharingError::InvalidRecord);
            }
        }
        let mut next = self.snapshot.clone();
        let slot = next.link_runs.entry(commit.folder_id).or_default();
        let rejection_matches_pending = slot.pending_request.as_ref().is_some_and(|request| {
            commit.rejected_request.as_ref().is_some_and(|rejected| {
                rejected.request_id == request.request_id
                    && rejected.requester_id == request.requester_id
            })
        });
        if commit.state.is_none() && !rejection_matches_pending {
            return Ok(());
        }
        if let Some(state) = &commit.state {
            slot.authoritative = Some(state.clone());
        }
        if slot.pending_request.as_ref().is_some_and(|request| {
            commit
                .state
                .as_ref()
                .is_some_and(|state| request.request_id == state.request_id)
                || commit.acknowledged_request_id == Some(request.request_id)
                || commit
                    .rejected_request
                    .as_ref()
                    .is_some_and(|rejected| rejected.request_id == request.request_id)
        }) {
            slot.pending_request = None;
        }
        slot.rejected_request = rejection_matches_pending
            .then(|| commit.rejected_request.clone())
            .flatten();
        if let (Some(report), Some(state)) = (&slot.pending_report, &commit.state) {
            let final_ack = state.generation > report.generation
                || (state.generation == report.generation
                    && (!state.phase.active()
                        || state
                            .destinations
                            .get(&self.engine.device_id())
                            .is_some_and(|destination| destination.result == report.result)));
            if final_ack {
                slot.pending_report = None;
            }
        }
        self.persist(next)
    }

    /// The caller authenticated reporter_id through the link-control envelope.
    pub fn receive_link_run_report(
        &mut self,
        report: &LinkRunReport,
        now_unix_ms: u64,
    ) -> Result<LinkRunState, SharingError> {
        self.reconcile_trust()?;
        if report.source_id != self.engine.device_id()
            || !report.source_index.is_valid()
            || !matches!(
                report.result,
                LinkRunDestinationResult::Succeeded | LinkRunDestinationResult::Failed
            )
            || report.ended_at_unix_ms == 0
            || report.ended_at_unix_ms > now_unix_ms
            || now_unix_ms == 0
        {
            return Err(SharingError::InvalidRecord);
        }
        validate_reporter_membership(&self.snapshot, report)?;
        let mut next = self.snapshot.clone();
        let state = next
            .link_runs
            .get_mut(&report.folder_id)
            .and_then(|slot| slot.authoritative.as_mut())
            .ok_or(SharingError::InvalidState)?;
        if report.generation < state.generation
            || (report.generation == state.generation && !state.phase.active())
        {
            return Ok(state.clone());
        }
        if state.generation != report.generation
            || state.settings_revision != report.settings_revision
            || state.source_index.as_ref() != Some(&report.source_index)
            || report.ended_at_unix_ms < state.started_at_unix_ms
            || report.ended_at_unix_ms >= state.deadline_unix_ms
        {
            return Err(SharingError::RunConflict);
        }
        let destination = state
            .destinations
            .get_mut(&report.reporter_id)
            .ok_or(SharingError::InvalidRecord)?;
        if destination.result.terminal() {
            if destination.result != report.result
                || destination.ended_at_unix_ms != Some(report.ended_at_unix_ms)
            {
                return Err(SharingError::InvalidRecord);
            }
            return Ok(state.clone());
        }
        if state.phase != LinkRunPhase::Running {
            return Err(SharingError::RunConflict);
        }
        destination.result = report.result;
        destination.ended_at_unix_ms = Some(report.ended_at_unix_ms);
        state.state_revision = next_state_revision(state.state_revision)?;
        finish_if_terminal(state, now_unix_ms);
        let result = state.clone();
        self.persist(next)?;
        Ok(result)
    }

    pub fn expire_link_runs(&mut self, now_unix_ms: u64) -> Result<bool, SharingError> {
        let owner = self.engine.device_id();
        let mut next = self.snapshot.clone();
        let source_folders = next
            .shares
            .iter()
            .filter(|share| !share.removed && share.offer.source_device_id == owner)
            .map(|share| share.offer.folder_id)
            .collect::<BTreeSet<_>>();
        let mut changed = false;
        for (folder_id, slot) in &mut next.link_runs {
            if !source_folders.contains(folder_id) {
                continue;
            }
            let Some(state) = slot.authoritative.as_mut() else {
                continue;
            };
            if state.phase.active() && now_unix_ms >= state.deadline_unix_ms {
                let ended_at = state.deadline_unix_ms;
                for destination in state.destinations.values_mut() {
                    if destination.result == LinkRunDestinationResult::Pending {
                        destination.result = LinkRunDestinationResult::TimedOut;
                        destination.ended_at_unix_ms = Some(ended_at);
                    }
                }
                state.phase = LinkRunPhase::Incomplete;
                state.ended_at_unix_ms = Some(ended_at);
                state.state_revision = next_state_revision(state.state_revision)?;
                changed = true;
            }
        }
        if changed {
            self.persist(next)?;
        }
        Ok(changed)
    }

    pub fn interrupt_unfinished_link_runs(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<bool, SharingError> {
        if now_unix_ms == 0 {
            return Err(SharingError::InvalidRecord);
        }
        let owner = self.engine.device_id();
        let mut next = self.snapshot.clone();
        let source_folders = next
            .shares
            .iter()
            .filter(|share| !share.removed && share.offer.source_device_id == owner)
            .map(|share| share.offer.folder_id)
            .collect::<BTreeSet<_>>();
        let mut changed = false;
        for (folder_id, slot) in &mut next.link_runs {
            if !source_folders.contains(folder_id) {
                continue;
            }
            let Some(state) = slot.authoritative.as_mut() else {
                continue;
            };
            if state.phase.active() {
                if now_unix_ms < state.started_at_unix_ms {
                    return Err(SharingError::InvalidRecord);
                }
                for destination in state.destinations.values_mut() {
                    if destination.result == LinkRunDestinationResult::Pending {
                        destination.result = LinkRunDestinationResult::Interrupted;
                        destination.ended_at_unix_ms = Some(now_unix_ms);
                    }
                }
                state.phase = LinkRunPhase::Interrupted;
                state.ended_at_unix_ms = Some(now_unix_ms);
                state.state_revision = next_state_revision(state.state_revision)?;
                changed = true;
            }
        }
        if changed {
            self.persist(next)?;
        }
        Ok(changed)
    }

    /// Remove destinations that no longer belong to an active source link.
    /// Call this after a source-side member removal or local member pause.
    pub fn reconcile_link_run_membership(
        &mut self,
        folder_id: Uuid,
        now_unix_ms: u64,
    ) -> Result<bool, SharingError> {
        if now_unix_ms == 0
            || !self.snapshot.shares.iter().any(|share| {
                share.offer.folder_id == folder_id
                    && share.offer.source_device_id == self.engine.device_id()
            })
        {
            return Err(SharingError::InvalidRecord);
        }
        let mut next = self.snapshot.clone();
        let changed = reconcile_membership(&mut next, self.engine.device_id(), now_unix_ms)?;
        if changed {
            self.persist(next)?;
        }
        Ok(changed)
    }

    pub fn admit_due_link_runs(
        &mut self,
        now_unix_ms: u64,
    ) -> Result<Vec<LinkRunState>, SharingError> {
        self.reconcile_trust()?;
        let owner = self.engine.device_id();
        let due = self
            .snapshot
            .link_settings
            .iter()
            .filter_map(|(folder_id, settings)| {
                if self.link_source(*folder_id).ok() != Some(owner)
                    || !settings.confirmed
                    || settings.settings.paused
                {
                    return None;
                }
                let interval_ms = match settings.settings.cadence {
                    LinkCadence::Continuous => CONTINUOUS_CHECK_INTERVAL_MS,
                    LinkCadence::Scheduled { interval_minutes } => {
                        u64::from(interval_minutes) * 60_000
                    }
                    LinkCadence::Manual => return None,
                };
                let base = self
                    .snapshot
                    .link_runs
                    .get(folder_id)
                    .and_then(|slot| slot.authoritative.as_ref())
                    .and_then(|state| state.ended_at_unix_ms)
                    .unwrap_or(settings.accepted_at_unix_ms);
                let due_at = base.checked_add(interval_ms)?;
                (base != 0 && now_unix_ms >= due_at).then_some(*folder_id)
            })
            .collect::<Vec<_>>();
        let mut next = self.snapshot.clone();
        let mut admitted = Vec::new();
        for folder_id in due {
            if next
                .link_runs
                .get(&folder_id)
                .and_then(|slot| slot.authoritative.as_ref())
                .is_some_and(|state| state.phase.active())
            {
                continue;
            }
            let settings_revision = next.link_settings[&folder_id].revision;
            let request = LinkRunRequest {
                folder_id,
                source_id: owner,
                requester_id: owner,
                request_id: Uuid::new_v4(),
                expected_generation: current_generation(&next, folder_id),
                settings_revision,
            };
            if let (LinkRunAdmission::Accepted(state), _) =
                admit_source_request(&mut next, &request, now_unix_ms, true)?
            {
                admitted.push(state);
            }
        }
        if !admitted.is_empty() {
            self.persist(next)?;
        }
        Ok(admitted)
    }

    pub fn active_batch_generations(
        &self,
        now_unix_ms: u64,
        observed_at: Instant,
    ) -> Result<BTreeMap<Uuid, u64>, SharingError> {
        self.store
            .payload()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        let mut result = BTreeMap::new();
        for (folder_id, slot) in &self.snapshot.link_runs {
            let Some(state) = &slot.authoritative else {
                continue;
            };
            if state.phase.active()
                && now_unix_ms < state.deadline_unix_ms
                && self.local_run_is_active(*folder_id, state, now_unix_ms, observed_at)
            {
                result.insert(*folder_id, state.generation);
            }
        }
        Ok(result)
    }

    pub fn link_run_work(
        &self,
        now_unix_ms: u64,
        observed_at: Instant,
    ) -> Result<Vec<LinkRunWorkItem>, SharingError> {
        let mut work = Vec::new();
        for (folder_id, slot) in &self.snapshot.link_runs {
            let Some(state) = &slot.authoritative else {
                continue;
            };
            if now_unix_ms >= state.deadline_unix_ms {
                continue;
            }
            let source_id = self.link_source(*folder_id)?;
            if source_id == self.engine.device_id()
                && state.phase == LinkRunPhase::Preparing
                && self.local_run_is_active(*folder_id, state, now_unix_ms, observed_at)
            {
                work.push(LinkRunWorkItem::PrepareSource {
                    folder_id: *folder_id,
                    generation: state.generation,
                    settings_revision: state.settings_revision,
                });
            } else if source_id != self.engine.device_id()
                && state.phase == LinkRunPhase::Running
                && self.local_run_is_active(*folder_id, state, now_unix_ms, observed_at)
            {
                work.push(LinkRunWorkItem::ObserveDestination {
                    folder_id: *folder_id,
                    generation: state.generation,
                    settings_revision: state.settings_revision,
                    source_id,
                    source_engine_id: self.link_source_engine_id(*folder_id)?,
                    source_index: state
                        .source_index
                        .clone()
                        .ok_or(SharingError::InvalidState)?,
                });
            }
        }
        Ok(work)
    }

    pub fn link_source_engine_id(&self, folder_id: Uuid) -> Result<EngineDeviceId, SharingError> {
        let source = self.link_source(folder_id)?;
        let raw = if source == self.engine.device_id() {
            self.installation.device_id().as_str()
        } else {
            self.snapshot
                .shares
                .iter()
                .find(|share| {
                    !share.removed
                        && share.offer.folder_id == folder_id
                        && share.offer.source_device_id == source
                })
                .ok_or(SharingError::InvalidRecord)?
                .offer
                .source_engine
                .engine_device_id
                .as_str()
        };
        EngineDeviceId::parse(raw).map_err(|_| SharingError::InvalidRecord)
    }

    pub(super) fn local_batch_allows_transfer_at(
        &self,
        folder_id: Uuid,
        now_unix_ms: u64,
        observed_at: Instant,
    ) -> bool {
        self.snapshot
            .link_runs
            .get(&folder_id)
            .and_then(|slot| slot.authoritative.as_ref())
            .is_some_and(|state| {
                state.phase.active()
                    && now_unix_ms < state.deadline_unix_ms
                    && self.local_run_is_active(folder_id, state, now_unix_ms, observed_at)
            })
    }

    fn local_run_is_active(
        &self,
        folder_id: Uuid,
        state: &LinkRunState,
        now_unix_ms: u64,
        observed_at: Instant,
    ) -> bool {
        let Ok(source_id) = self.link_source(folder_id) else {
            return false;
        };
        let Some(settings) = self.snapshot.link_settings.get(&folder_id) else {
            return false;
        };
        if !settings.confirmed
            || settings.revision != state.settings_revision
            || settings.settings.paused
            || now_unix_ms >= state.deadline_unix_ms
            || !self.conditions_allow(settings.settings.android_conditions, observed_at)
        {
            return false;
        }
        if source_id == self.engine.device_id() {
            return state.phase.active();
        }
        state.phase == LinkRunPhase::Running
            && state.source_index.is_some()
            && state
                .destinations
                .get(&self.engine.device_id())
                .is_some_and(|destination| destination.result == LinkRunDestinationResult::Pending)
            && self
                .snapshot
                .link_runs
                .get(&folder_id)
                .is_none_or(|slot| slot.pending_report.is_none())
    }

    pub(super) fn conditions_allow(&self, required: AndroidLinkConditions, now: Instant) -> bool {
        if !self.android_host || (!required.wifi_only && !required.charging_only) {
            return true;
        }
        let Some(observed) = self.android_condition_observation else {
            return false;
        };
        if now.saturating_duration_since(observed.observed_at) > ANDROID_CONDITION_TTL {
            return false;
        }
        (!required.wifi_only || observed.wifi_connected)
            && (!required.charging_only || observed.charging)
    }
}

pub(super) fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| u64::try_from(value.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

fn admit_source_request(
    snapshot: &mut Snapshot,
    request: &LinkRunRequest,
    now_unix_ms: u64,
    allow_automatic_continuous: bool,
) -> Result<(LinkRunAdmission, bool), SharingError> {
    if now_unix_ms == 0 {
        return Err(SharingError::InvalidRecord);
    }
    validate_request_membership(snapshot, request)?;
    let settings = snapshot
        .link_settings
        .get(&request.folder_id)
        .ok_or(SharingError::InvalidState)?;
    if !settings.confirmed
        || settings.settings.paused
        || (settings.settings.cadence == LinkCadence::Continuous && !allow_automatic_continuous)
        || now_unix_ms < settings.accepted_at_unix_ms
    {
        return Err(SharingError::InvalidState);
    }
    let settings_revision = settings.revision;
    if let Some(current) = snapshot
        .link_runs
        .get(&request.folder_id)
        .and_then(|slot| slot.authoritative.as_ref())
    {
        if current.request_id == request.request_id
            && current.requested_by == request.requester_id
            && request.expected_generation.checked_add(1) == Some(current.generation)
            && request.settings_revision == current.settings_revision
        {
            return Ok((LinkRunAdmission::AlreadyRunning(current.clone()), false));
        }
        if current.phase.active() {
            return Ok((LinkRunAdmission::AlreadyRunning(current.clone()), false));
        }
    }
    let reject = if settings_revision != request.settings_revision {
        Some(LinkRunRejectionReason::SettingsChanged)
    } else if current_generation(snapshot, request.folder_id) != request.expected_generation {
        Some(LinkRunRejectionReason::GenerationChanged)
    } else {
        None
    };
    let destinations = snapshot
        .shares
        .iter()
        .filter(|share| {
            !share.removed
                && !share.paused
                && share.commit.is_some()
                && share.offer.folder_id == request.folder_id
                && share.offer.source_device_id == request.source_id
        })
        .map(|share| {
            (
                share.offer.target_device_id,
                LinkRunDestinationState {
                    result: LinkRunDestinationResult::Pending,
                    ended_at_unix_ms: None,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let slot = snapshot.link_runs.entry(request.folder_id).or_default();
    if let Some(reason) = reject {
        let rejection = LinkRunRejection {
            requester_id: request.requester_id,
            request_id: request.request_id,
            reason,
        };
        slot.rejected_request = Some(rejection.clone());
        return Ok((LinkRunAdmission::Rejected(rejection), true));
    }
    let generation = request
        .expected_generation
        .checked_add(1)
        .ok_or(SharingError::LimitExceeded)?;
    let state_revision = slot
        .authoritative
        .as_ref()
        .map_or(Ok(1), |state| next_state_revision(state.state_revision))?;
    let deadline_unix_ms = now_unix_ms
        .checked_add(LINK_RUN_DEADLINE_MS)
        .ok_or(SharingError::LimitExceeded)?;
    if destinations.is_empty() {
        return Err(SharingError::InvalidRecord);
    }
    let state = LinkRunState {
        generation,
        state_revision,
        settings_revision: request.settings_revision,
        request_id: request.request_id,
        requested_by: request.requester_id,
        phase: LinkRunPhase::Preparing,
        started_at_unix_ms: now_unix_ms,
        deadline_unix_ms,
        ended_at_unix_ms: None,
        source_index: None,
        destinations,
    };
    slot.authoritative = Some(state.clone());
    slot.pending_request = None;
    slot.rejected_request = None;
    Ok((LinkRunAdmission::Accepted(state), true))
}

fn validate_request_membership(
    snapshot: &Snapshot,
    request: &LinkRunRequest,
) -> Result<(), SharingError> {
    if request.request_id.is_nil()
        || link_source_in_snapshot(&snapshot.shares, request.folder_id) != Some(request.source_id)
        || !snapshot.shares.iter().any(|share| {
            !share.removed
                && share.commit.is_some()
                && share.offer.folder_id == request.folder_id
                && share.offer.source_device_id == request.source_id
                && (request.requester_id == request.source_id
                    || share.offer.target_device_id == request.requester_id)
        })
    {
        return Err(SharingError::InvalidRecord);
    }
    Ok(())
}

fn validate_reporter_membership(
    snapshot: &Snapshot,
    report: &LinkRunReport,
) -> Result<(), SharingError> {
    if !snapshot.shares.iter().any(|share| {
        !share.removed
            && share.commit.is_some()
            && share.offer.folder_id == report.folder_id
            && share.offer.source_device_id == report.source_id
            && share.offer.target_device_id == report.reporter_id
    }) {
        return Err(SharingError::InvalidRecord);
    }
    Ok(())
}

fn validate_commit_for_local(
    snapshot: &Snapshot,
    owner: DeviceId,
    commit: &LinkRunCommit,
) -> Result<(), SharingError> {
    if commit.target_id != owner
        || commit.source_id == owner
        || link_source_in_snapshot(&snapshot.shares, commit.folder_id) != Some(commit.source_id)
        || !snapshot.shares.iter().any(|share| {
            !share.removed
                && share.commit.is_some()
                && share.offer.folder_id == commit.folder_id
                && share.offer.source_device_id == commit.source_id
                && share.offer.target_device_id == owner
        })
    {
        return Err(SharingError::InvalidRecord);
    }
    if let Some(state) = &commit.state {
        if state.request_id.is_nil()
            || state.generation == 0
            || state.state_revision == 0
            || !state.destinations.contains_key(&owner)
            || state.destinations.keys().any(|id| *id != owner)
        {
            return Err(SharingError::InvalidRecord);
        }
        validate_run_state(state)?;
    } else if commit.rejected_request.is_none() {
        return Err(SharingError::InvalidRecord);
    }
    if commit
        .acknowledged_request_id
        .is_some_and(|request_id| request_id.is_nil())
    {
        return Err(SharingError::InvalidRecord);
    }
    if commit
        .rejected_request
        .as_ref()
        .is_some_and(|rejected| rejected.requester_id != owner || rejected.request_id.is_nil())
    {
        return Err(SharingError::InvalidRecord);
    }
    Ok(())
}

fn commit_for_target(
    snapshot: &Snapshot,
    folder_id: Uuid,
    target_id: DeviceId,
) -> Result<LinkRunCommit, SharingError> {
    let source_id =
        link_source_in_snapshot(&snapshot.shares, folder_id).ok_or(SharingError::InvalidRecord)?;
    let slot = snapshot
        .link_runs
        .get(&folder_id)
        .ok_or(SharingError::InvalidState)?;
    let state = slot
        .authoritative
        .clone()
        .map(|mut state| {
            let destination = state
                .destinations
                .get(&target_id)
                .cloned()
                .ok_or(SharingError::InvalidRecord)?;
            state.destinations = BTreeMap::from([(target_id, destination)]);
            Ok(state)
        })
        .transpose()?;
    let rejected_request = slot
        .rejected_request
        .clone()
        .filter(|rejected| rejected.requester_id == target_id);
    if state.is_none() && rejected_request.is_none() {
        return Err(SharingError::InvalidState);
    }
    Ok(LinkRunCommit {
        folder_id,
        source_id,
        target_id,
        state,
        acknowledged_request_id: None,
        rejected_request,
    })
}

fn finish_if_terminal(state: &mut LinkRunState, now_unix_ms: u64) {
    if state
        .destinations
        .values()
        .all(|destination| destination.result.terminal())
    {
        state.phase = if state
            .destinations
            .values()
            .all(|destination| destination.result == LinkRunDestinationResult::Succeeded)
        {
            LinkRunPhase::Succeeded
        } else {
            LinkRunPhase::Incomplete
        };
        state.ended_at_unix_ms = Some(now_unix_ms);
    }
}

fn next_state_revision(current: u64) -> Result<u64, SharingError> {
    current.checked_add(1).ok_or(SharingError::LimitExceeded)
}

fn current_generation(snapshot: &Snapshot, folder_id: Uuid) -> u64 {
    snapshot
        .link_runs
        .get(&folder_id)
        .map_or(0, current_generation_from_slot)
}

fn current_generation_from_slot(slot: &LinkRunLinkState) -> u64 {
    slot.authoritative
        .as_ref()
        .map_or(0, |state| state.generation)
}

fn link_source_in_snapshot(shares: &[Share], folder_id: Uuid) -> Option<DeviceId> {
    shares
        .iter()
        .find(|share| {
            !share.removed
                && share.offer.folder_id == folder_id
                && share.offer.link_policy.is_some()
        })
        .map(|share| share.offer.source_device_id)
}

pub(super) fn append_run_deliveries(
    snapshot: &Snapshot,
    owner: DeviceId,
    result: &mut Vec<FolderShareDelivery>,
) {
    for share in snapshot.shares.iter().filter(|share| {
        !share.removed && share.commit.is_some() && share.offer.link_policy.is_some()
    }) {
        let Some(slot) = snapshot.link_runs.get(&share.offer.folder_id) else {
            continue;
        };
        let record = if share.offer.source_device_id == owner {
            let Ok(commit) = commit_for_target(
                snapshot,
                share.offer.folder_id,
                share.offer.target_device_id,
            ) else {
                continue;
            };
            FolderShareRecord::RunCommit(commit)
        } else if let Some(report) = &slot.pending_report {
            FolderShareRecord::RunReport(report.clone())
        } else if let Some(request) = &slot.pending_request {
            FolderShareRecord::RunRequest(request.clone())
        } else {
            continue;
        };
        result.push(FolderShareDelivery {
            peer_transport: share.peer_transport.clone(),
            record,
        });
    }
}

pub(super) fn cancel_for_settings_change(
    snapshot: &mut Snapshot,
    folder_id: Uuid,
    now_unix_ms: u64,
) -> Result<(), SharingError> {
    let Some(slot) = snapshot.link_runs.get_mut(&folder_id) else {
        return Ok(());
    };
    slot.pending_request = None;
    slot.pending_report = None;
    let Some(state) = slot.authoritative.as_mut() else {
        return Ok(());
    };
    if state.phase.active() {
        for destination in state.destinations.values_mut() {
            if destination.result == LinkRunDestinationResult::Pending {
                destination.result = LinkRunDestinationResult::Cancelled;
                destination.ended_at_unix_ms = Some(now_unix_ms);
            }
        }
        state.phase = LinkRunPhase::Cancelled;
        state.ended_at_unix_ms = Some(now_unix_ms);
        state.state_revision = next_state_revision(state.state_revision)?;
    }
    Ok(())
}

pub(super) fn reconcile_run_states(snapshot: &mut Snapshot) {
    let active = snapshot
        .link_settings
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    snapshot
        .link_runs
        .retain(|folder_id, _| active.contains(folder_id));
}

pub(super) fn reconcile_membership(
    snapshot: &mut Snapshot,
    owner: DeviceId,
    now_unix_ms: u64,
) -> Result<bool, SharingError> {
    let retained = snapshot
        .shares
        .iter()
        .filter(|share| {
            !share.removed
                && !share.paused
                && share.commit.is_some()
                && share.offer.source_device_id == owner
        })
        .map(|share| (share.offer.folder_id, share.offer.target_device_id))
        .collect::<BTreeSet<_>>();
    let source_folders = snapshot
        .shares
        .iter()
        .filter(|share| share.offer.source_device_id == owner)
        .map(|share| share.offer.folder_id)
        .collect::<BTreeSet<_>>();
    let mut changed = false;
    for (folder_id, slot) in &mut snapshot.link_runs {
        if !source_folders.contains(folder_id) {
            continue;
        }
        let Some(state) = slot.authoritative.as_mut() else {
            continue;
        };
        if !state.phase.active() {
            continue;
        }
        let mut state_changed = false;
        for (peer_id, destination) in &mut state.destinations {
            if destination.result == LinkRunDestinationResult::Pending
                && !retained.contains(&(*folder_id, *peer_id))
            {
                destination.result = LinkRunDestinationResult::Cancelled;
                destination.ended_at_unix_ms = Some(now_unix_ms);
                state_changed = true;
            }
        }
        if state_changed {
            state.state_revision = next_state_revision(state.state_revision)?;
            finish_if_terminal(state, now_unix_ms);
            changed = true;
        }
    }
    Ok(changed)
}

pub(super) fn validate_run_states(snapshot: &Snapshot) -> Result<(), SharingError> {
    if snapshot.link_runs.len() > MAX_SHARES {
        return Err(SharingError::LimitExceeded);
    }
    for (folder_id, slot) in &snapshot.link_runs {
        let source = link_source_in_snapshot(&snapshot.shares, *folder_id)
            .ok_or(SharingError::InvalidState)?;
        if let Some(state) = &slot.authoritative {
            validate_run_state(state)?;
            let current_settings = snapshot
                .link_settings
                .get(folder_id)
                .ok_or(SharingError::InvalidState)?;
            if state.settings_revision > current_settings.revision
                || (state.phase.active()
                    && (state.settings_revision != current_settings.revision
                        || !current_settings.confirmed
                        || current_settings.settings.paused))
            {
                return Err(SharingError::InvalidState);
            }
            let known = snapshot
                .shares
                .iter()
                .filter(|share| share.offer.folder_id == *folder_id)
                .map(|share| share.offer.target_device_id)
                .chain(
                    snapshot
                        .tombstones
                        .iter()
                        .filter(|item| item.folder_id == *folder_id)
                        .map(|item| item.peer_id),
                )
                .collect::<BTreeSet<_>>();
            if state.destinations.keys().any(|id| !known.contains(id)) {
                return Err(SharingError::InvalidState);
            }
        }
        if let Some(request) = &slot.pending_request
            && (request.folder_id != *folder_id
                || request.source_id != source
                || request.requester_id != snapshot.owner.device_id
                || request.request_id.is_nil()
                || source == snapshot.owner.device_id)
        {
            return Err(SharingError::InvalidState);
        }
        if let Some(rejected) = &slot.rejected_request {
            let known_requester = rejected.requester_id == source
                || snapshot.shares.iter().any(|share| {
                    share.offer.folder_id == *folder_id
                        && share.offer.source_device_id == source
                        && share.offer.target_device_id == rejected.requester_id
                });
            if rejected.request_id.is_nil()
                || !known_requester
                || (source != snapshot.owner.device_id
                    && rejected.requester_id != snapshot.owner.device_id)
            {
                return Err(SharingError::InvalidState);
            }
        }
        if let Some(report) = &slot.pending_report
            && (report.folder_id != *folder_id
                || report.source_id != source
                || report.reporter_id != snapshot.owner.device_id
                || source == snapshot.owner.device_id
                || !report.source_index.is_valid()
                || !matches!(
                    report.result,
                    LinkRunDestinationResult::Succeeded | LinkRunDestinationResult::Failed
                ))
        {
            return Err(SharingError::InvalidState);
        }
    }
    Ok(())
}

fn validate_run_state(state: &LinkRunState) -> Result<(), SharingError> {
    if state.generation == 0
        || state.state_revision == 0
        || state.request_id.is_nil()
        || state.started_at_unix_ms == 0
        || state.deadline_unix_ms
            != state
                .started_at_unix_ms
                .checked_add(LINK_RUN_DEADLINE_MS)
                .ok_or(SharingError::InvalidState)?
        || state
            .ended_at_unix_ms
            .is_some_and(|ended| ended < state.started_at_unix_ms)
        || state.destinations.is_empty()
        || state.destinations.len() > MAX_SHARES
        || state.destinations.values().any(|destination| {
            destination.result.terminal() != destination.ended_at_unix_ms.is_some()
                || destination
                    .ended_at_unix_ms
                    .is_some_and(|ended| ended < state.started_at_unix_ms)
        })
    {
        return Err(SharingError::InvalidState);
    }
    match state.phase {
        LinkRunPhase::Preparing => {
            if state.source_index.is_some() || state.ended_at_unix_ms.is_some() {
                return Err(SharingError::InvalidState);
            }
        }
        LinkRunPhase::Running => {
            if state
                .source_index
                .as_ref()
                .is_none_or(|index| !index.is_valid())
                || state.ended_at_unix_ms.is_some()
            {
                return Err(SharingError::InvalidState);
            }
        }
        LinkRunPhase::Succeeded => {
            if state
                .source_index
                .as_ref()
                .is_none_or(|index| !index.is_valid())
                || state.ended_at_unix_ms.is_none()
                || state
                    .destinations
                    .values()
                    .any(|destination| destination.result != LinkRunDestinationResult::Succeeded)
            {
                return Err(SharingError::InvalidState);
            }
        }
        LinkRunPhase::Incomplete | LinkRunPhase::Interrupted | LinkRunPhase::Cancelled => {
            if state.ended_at_unix_ms.is_none()
                || state
                    .destinations
                    .values()
                    .any(|destination| !destination.result.terminal())
            {
                return Err(SharingError::InvalidState);
            }
        }
    }
    Ok(())
}

pub(super) fn summary(
    snapshot: &Snapshot,
    folder_id: Uuid,
    owner: DeviceId,
) -> Option<LinkRunSummary> {
    let settings = snapshot.link_settings.get(&folder_id)?;
    let empty = LinkRunLinkState::default();
    let slot = snapshot.link_runs.get(&folder_id).unwrap_or(&empty);
    let next_due_at_unix_ms = if link_source_in_snapshot(&snapshot.shares, folder_id) == Some(owner)
    {
        let LinkCadence::Scheduled { interval_minutes } = settings.settings.cadence else {
            return Some(summary_from(slot, settings.revision, None));
        };
        if slot
            .authoritative
            .as_ref()
            .is_some_and(|state| state.phase.active())
        {
            None
        } else {
            slot.authoritative
                .as_ref()
                .and_then(|state| state.ended_at_unix_ms)
                .or(Some(settings.accepted_at_unix_ms))
                .and_then(|base| base.checked_add(u64::from(interval_minutes) * 60_000))
        }
    } else {
        None
    };
    Some(summary_from(slot, settings.revision, next_due_at_unix_ms))
}

fn summary_from(
    slot: &LinkRunLinkState,
    current_settings_revision: u64,
    next_due_at_unix_ms: Option<u64>,
) -> LinkRunSummary {
    let state = slot.authoritative.as_ref();
    LinkRunSummary {
        generation: state.map_or(0, |state| state.generation),
        state_revision: state.map_or(0, |state| state.state_revision),
        settings_revision: state.map_or(current_settings_revision, |state| state.settings_revision),
        phase: state.map(|state| state.phase),
        started_at_unix_ms: state.map(|state| state.started_at_unix_ms),
        deadline_unix_ms: state.map(|state| state.deadline_unix_ms),
        ended_at_unix_ms: state.and_then(|state| state.ended_at_unix_ms),
        next_due_at_unix_ms,
        pending_request: slot
            .pending_request
            .as_ref()
            .map(|request| LinkRunRequestSummary {
                request_id: request.request_id,
                requester_id: request.requester_id,
            }),
        rejected_request: slot.rejected_request.clone(),
        destinations: state
            .into_iter()
            .flat_map(|state| &state.destinations)
            .map(|(peer_id, destination)| LinkRunDestinationSummary {
                peer_id: *peer_id,
                result: destination.result,
                ended_at_unix_ms: destination.ended_at_unix_ms,
            })
            .collect(),
    }
}
