//! Link-wide decisions carried by the existing authenticated control channel.
//!
//! The source serializes revisions. Destinations retain an offline request
//! until a source-authenticated revision accepts it or reports a conflict.
//! Original signed invitations remain immutable.

use super::*;

pub const MIN_SCHEDULE_INTERVAL_MINUTES: u32 = 15;
pub const MAX_SCHEDULE_INTERVAL_MINUTES: u32 = 525_600;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LinkCadence {
    Manual,
    #[default]
    Continuous,
    Scheduled {
        interval_minutes: u32,
    },
}

impl LinkCadence {
    pub fn is_valid(self) -> bool {
        match self {
            Self::Manual | Self::Continuous => true,
            Self::Scheduled { interval_minutes } => (MIN_SCHEDULE_INTERVAL_MINUTES
                ..=MAX_SCHEDULE_INTERVAL_MINUTES)
                .contains(&interval_minutes),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AndroidLinkConditions {
    pub wifi_only: bool,
    pub charging_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FolderLinkSettings {
    pub deletion_policy: covalent_protocol::FolderLinkPolicy,
    pub paused: bool,
    #[serde(default)]
    pub cadence: LinkCadence,
    #[serde(default)]
    pub android_conditions: AndroidLinkConditions,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkSettingsRequest {
    pub folder_id: Uuid,
    pub source_id: DeviceId,
    pub requester_id: DeviceId,
    pub change_id: Uuid,
    pub expected_revision: u64,
    pub settings: FolderLinkSettings,
}

/// Valid only inside an authenticated envelope from source_id to target_id.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkSettingsCommit {
    pub folder_id: Uuid,
    pub source_id: DeviceId,
    pub target_id: DeviceId,
    pub revision: u64,
    pub settings: FolderLinkSettings,
    pub change_id: Uuid,
    pub changed_by: DeviceId,
    #[serde(default)]
    pub accepted_at_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkSettingsState {
    pub revision: u64,
    pub settings: FolderLinkSettings,
    pub change_id: Uuid,
    pub changed_by: DeviceId,
    /// Source-issued acceptance time. Historical continuous settings may be zero.
    #[serde(default)]
    pub accepted_at_unix_ms: u64,
    /// A new destination waits for the source's current revision before any
    /// transfer. Its invitation may precede a later settings change.
    pub confirmed: bool,
    pub pending_change: Option<LinkSettingsRequest>,
    pub conflicted_change: Option<LinkSettingsRequest>,
}

impl LinkSettingsState {
    fn commit(
        &self,
        folder_id: Uuid,
        source_id: DeviceId,
        target_id: DeviceId,
    ) -> LinkSettingsCommit {
        LinkSettingsCommit {
            folder_id,
            source_id,
            target_id,
            revision: self.revision,
            settings: self.settings,
            change_id: self.change_id,
            changed_by: self.changed_by,
            accepted_at_unix_ms: self.accepted_at_unix_ms,
        }
    }
}

impl FolderSharingJournal {
    pub fn request_link_settings(
        &mut self,
        folder_id: Uuid,
        change_id: Uuid,
        expected_revision: u64,
        settings: FolderLinkSettings,
    ) -> Result<(), SharingError> {
        let accepted_at = self
            .snapshot
            .link_settings
            .get(&folder_id)
            .map_or(0, |state| state.accepted_at_unix_ms);
        self.request_link_settings_at(
            folder_id,
            change_id,
            expected_revision,
            settings,
            accepted_at,
        )
    }

    pub fn request_link_settings_at(
        &mut self,
        folder_id: Uuid,
        change_id: Uuid,
        expected_revision: u64,
        settings: FolderLinkSettings,
        now_unix_ms: u64,
    ) -> Result<(), SharingError> {
        self.reconcile_trust()?;
        let source_id = self.link_source(folder_id)?;
        let state = self
            .snapshot
            .link_settings
            .get(&folder_id)
            .ok_or(SharingError::InvalidState)?;
        if change_id.is_nil() || !state.confirmed || !settings.is_valid() {
            return Err(SharingError::InvalidRecord);
        }
        let request = LinkSettingsRequest {
            folder_id,
            source_id,
            requester_id: self.engine.device_id(),
            change_id,
            expected_revision,
            settings,
        };
        if state.change_id == change_id {
            return if state.settings == settings
                && state.changed_by == request.requester_id
                && expected_revision.checked_add(1) == Some(state.revision)
            {
                Ok(())
            } else {
                Err(SharingError::InvalidRecord)
            };
        }
        if expected_revision != state.revision {
            return Err(SharingError::SettingsConflict);
        }
        if let Some(pending) = &state.pending_change {
            return if pending == &request {
                Ok(())
            } else {
                Err(SharingError::SettingsPending)
            };
        }
        if source_id == self.engine.device_id() {
            self.commit_link_request(&request, now_unix_ms)?;
        } else {
            if !self.snapshot.shares.iter().any(|s| {
                !s.removed
                    && s.commit.is_some()
                    && s.offer.folder_id == folder_id
                    && s.offer.source_device_id == source_id
            }) {
                return Err(SharingError::InvalidRecord);
            }
            let mut next = self.snapshot.clone();
            let state = next
                .link_settings
                .get_mut(&folder_id)
                .ok_or(SharingError::InvalidState)?;
            state.pending_change = Some(request);
            state.conflicted_change = None;
            self.persist(next)?;
        }
        Ok(())
    }

    /// The caller verified requester_id through the folder-control envelope.
    pub fn receive_link_settings_request(
        &mut self,
        request: &LinkSettingsRequest,
    ) -> Result<LinkSettingsCommit, SharingError> {
        let accepted_at = self
            .snapshot
            .link_settings
            .get(&request.folder_id)
            .map_or(0, |state| state.accepted_at_unix_ms);
        self.receive_link_settings_request_at(request, accepted_at)
    }

    pub fn receive_link_settings_request_at(
        &mut self,
        request: &LinkSettingsRequest,
        now_unix_ms: u64,
    ) -> Result<LinkSettingsCommit, SharingError> {
        self.reconcile_trust()?;
        if request.source_id != self.engine.device_id()
            || request.change_id.is_nil()
            || self.link_source(request.folder_id)? != self.engine.device_id()
            || !request.settings.is_valid()
        {
            return Err(SharingError::InvalidRecord);
        }
        let peer = self.trusted_peer(request.requester_id)?;
        if !self.snapshot.shares.iter().any(|s| {
            !s.removed
                && s.commit.is_some()
                && s.offer.folder_id == request.folder_id
                && s.offer.target_device_id == request.requester_id
                && peer.matches(s)
        }) {
            return Err(SharingError::UntrustedPeer);
        }
        let current = self
            .snapshot
            .link_settings
            .get(&request.folder_id)
            .ok_or(SharingError::InvalidState)?;
        if current.change_id == request.change_id {
            if current.settings != request.settings
                || current.changed_by != request.requester_id
                || request.expected_revision.checked_add(1) != Some(current.revision)
            {
                return Err(SharingError::InvalidRecord);
            }
        } else if current.revision == request.expected_revision {
            self.commit_link_request(request, now_unix_ms)?;
        }
        // An old request receives the current revision, without overwriting it.
        // The requesting device retains its attempted values as a conflict.
        Ok(self
            .snapshot
            .link_settings
            .get(&request.folder_id)
            .ok_or(SharingError::InvalidState)?
            .commit(
                request.folder_id,
                self.engine.device_id(),
                request.requester_id,
            ))
    }

    fn commit_link_request(
        &mut self,
        request: &LinkSettingsRequest,
        now_unix_ms: u64,
    ) -> Result<(), SharingError> {
        if now_unix_ms == 0 {
            return Err(SharingError::InvalidRecord);
        }
        let mut next = self.snapshot.clone();
        let state = next
            .link_settings
            .get_mut(&request.folder_id)
            .ok_or(SharingError::InvalidState)?;
        if state.revision != request.expected_revision || now_unix_ms < state.accepted_at_unix_ms {
            return Err(SharingError::SettingsConflict);
        }
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or(SharingError::LimitExceeded)?;
        state.settings = request.settings;
        state.change_id = request.change_id;
        state.changed_by = request.requester_id;
        state.accepted_at_unix_ms = now_unix_ms;
        state.pending_change = None;
        state.conflicted_change = None;
        link_runs::cancel_for_settings_change(&mut next, request.folder_id, now_unix_ms)?;
        self.persist(next)
    }

    /// The caller verified source_id through the folder-control envelope.
    pub fn receive_link_settings_commit(
        &mut self,
        commit: &LinkSettingsCommit,
    ) -> Result<(), SharingError> {
        self.reconcile_trust()?;
        if commit.target_id != self.engine.device_id()
            || commit.source_id == self.engine.device_id()
            || self.link_source(commit.folder_id)? != commit.source_id
            || (commit.revision == 0) != commit.change_id.is_nil()
            || !commit.settings.is_valid()
            || commit.accepted_at_unix_ms == 0
        {
            return Err(SharingError::InvalidRecord);
        }
        let peer = self.trusted_peer(commit.source_id)?;
        if !self.snapshot.shares.iter().any(|s| {
            !s.removed
                && s.commit.is_some()
                && s.offer.folder_id == commit.folder_id
                && peer.matches(s)
        }) {
            return Err(SharingError::UntrustedPeer);
        }
        let current = self
            .snapshot
            .link_settings
            .get(&commit.folder_id)
            .ok_or(SharingError::InvalidState)?;
        if commit.revision < current.revision {
            // A delayed authenticated older commit cannot roll settings back.
            return Ok(());
        }
        if commit.revision > current.revision
            && commit.accepted_at_unix_ms < current.accepted_at_unix_ms
        {
            return Err(SharingError::InvalidRecord);
        }
        if commit.revision == 0 && commit.changed_by != commit.source_id {
            return Err(SharingError::InvalidRecord);
        }
        if current.confirmed
            && commit.revision == current.revision
            && (commit.settings != current.settings
                || commit.change_id != current.change_id
                || commit.changed_by != current.changed_by
                || commit.accepted_at_unix_ms != current.accepted_at_unix_ms)
        {
            return Err(SharingError::InvalidRecord);
        }
        let mut next = self.snapshot.clone();
        let state = next
            .link_settings
            .get_mut(&commit.folder_id)
            .ok_or(SharingError::InvalidState)?;
        state.revision = commit.revision;
        state.settings = commit.settings;
        state.change_id = commit.change_id;
        state.changed_by = commit.changed_by;
        state.accepted_at_unix_ms = commit.accepted_at_unix_ms;
        state.confirmed = true;
        if let Some(pending) = &state.pending_change {
            if pending.change_id == commit.change_id && pending.requester_id == commit.changed_by {
                if pending.settings != commit.settings
                    || pending.expected_revision.checked_add(1) != Some(commit.revision)
                {
                    return Err(SharingError::InvalidRecord);
                }
                state.pending_change = None;
                state.conflicted_change = None;
            } else if pending.expected_revision < commit.revision {
                state.conflicted_change = state.pending_change.take();
            }
        }
        if state == current {
            return Ok(());
        }
        link_runs::cancel_for_settings_change(
            &mut next,
            commit.folder_id,
            commit.accepted_at_unix_ms,
        )?;
        self.persist(next)
    }

    pub(super) fn link_source(&self, folder_id: Uuid) -> Result<DeviceId, SharingError> {
        self.snapshot
            .shares
            .iter()
            .find(|s| !s.removed && s.offer.folder_id == folder_id && s.offer.link_policy.is_some())
            .map(|s| s.offer.source_device_id)
            .ok_or(SharingError::InvalidRecord)
    }

    pub(super) fn link_allows_transfer(&self, share: &Share) -> bool {
        self.link_allows_transfer_at(share, link_runs::current_unix_ms(), Instant::now())
    }

    pub(super) fn link_allows_transfer_at(
        &self,
        share: &Share,
        now_unix_ms: u64,
        observed_at: Instant,
    ) -> bool {
        share.offer.link_policy.is_none()
            || self
                .snapshot
                .link_settings
                .get(&share.offer.folder_id)
                .is_some_and(|state| {
                    state.confirmed
                        && !state.settings.paused
                        && match state.settings.cadence {
                            LinkCadence::Continuous => self
                                .conditions_allow(state.settings.android_conditions, observed_at),
                            LinkCadence::Manual | LinkCadence::Scheduled { .. } => self
                                .local_batch_allows_transfer_at(
                                    share.offer.folder_id,
                                    now_unix_ms,
                                    observed_at,
                                ),
                        }
                })
    }

    pub(super) fn append_link_deliveries(&self, result: &mut Vec<FolderShareDelivery>) {
        for share in self
            .snapshot
            .shares
            .iter()
            .filter(|s| !s.removed && s.commit.is_some())
        {
            let Some(state) = self.snapshot.link_settings.get(&share.offer.folder_id) else {
                continue;
            };
            let record = if share.offer.source_device_id == self.engine.device_id() {
                FolderShareRecord::SettingsCommit(state.commit(
                    share.offer.folder_id,
                    self.engine.device_id(),
                    share.peer_identity.device_id,
                ))
            } else if let Some(pending) = &state.pending_change {
                FolderShareRecord::SettingsRequest(pending.clone())
            } else {
                continue;
            };
            result.push(FolderShareDelivery {
                peer_transport: share.peer_transport.clone(),
                record,
            });
        }
    }
}

pub(super) fn reconcile_link_states(snapshot: &mut Snapshot) {
    let owner = snapshot.owner.device_id;
    let active: BTreeSet<_> = snapshot
        .shares
        .iter()
        .filter(|s| !s.removed && s.offer.link_policy.is_some())
        .map(|s| s.offer.folder_id)
        .collect();
    snapshot.link_settings.retain(|id, _| active.contains(id));
    for share in snapshot.shares.iter().filter(|s| !s.removed) {
        if let Some(policy) = share.offer.link_policy {
            snapshot
                .link_settings
                .entry(share.offer.folder_id)
                .or_insert_with(|| LinkSettingsState {
                    revision: 0,
                    settings: FolderLinkSettings {
                        deletion_policy: policy,
                        paused: false,
                        cadence: LinkCadence::Continuous,
                        android_conditions: AndroidLinkConditions::default(),
                    },
                    change_id: Uuid::nil(),
                    changed_by: share.offer.source_device_id,
                    accepted_at_unix_ms: share.offer.issued_at_unix_ms,
                    confirmed: owner == share.offer.source_device_id,
                    pending_change: None,
                    conflicted_change: None,
                });
        }
    }
}

pub(super) fn validate_link_states(snapshot: &Snapshot) -> Result<(), SharingError> {
    let owner = snapshot.owner.device_id;
    for (id, state) in &snapshot.link_settings {
        let source = snapshot
            .shares
            .iter()
            .find(|s| !s.removed && s.offer.folder_id == *id && s.offer.link_policy.is_some())
            .ok_or(SharingError::InvalidState)?
            .offer
            .source_device_id;
        if (state.revision == 0) != state.change_id.is_nil()
            || !state.settings.is_valid()
            || (matches!(state.settings.cadence, LinkCadence::Scheduled { .. })
                && state.accepted_at_unix_ms == 0)
            || (source == owner && (!state.confirmed || state.pending_change.is_some()))
            || (state.revision == 0 && state.changed_by != source)
            || (!state.confirmed && state.revision != 0)
        {
            return Err(SharingError::InvalidState);
        }
        for pending in [&state.pending_change, &state.conflicted_change]
            .into_iter()
            .flatten()
        {
            if source == owner
                || pending.source_id != source
                || pending.requester_id != owner
                || pending.folder_id != *id
                || pending.change_id.is_nil()
                || pending.expected_revision > state.revision
            {
                return Err(SharingError::InvalidState);
            }
        }
    }
    for share in snapshot
        .shares
        .iter()
        .filter(|s| !s.removed && s.offer.link_policy.is_some())
    {
        if !snapshot.link_settings.contains_key(&share.offer.folder_id) {
            return Err(SharingError::InvalidState);
        }
    }
    Ok(())
}

impl FolderLinkSettings {
    pub fn is_valid(self) -> bool {
        self.cadence.is_valid()
    }
}
