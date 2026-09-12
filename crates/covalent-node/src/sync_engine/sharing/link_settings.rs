//! Link-wide decisions carried by the existing authenticated control channel.
//!
//! The source serializes revisions. Destinations retain an offline request
//! until a source-authenticated revision accepts it or reports a conflict.
//! Original signed invitations remain immutable.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FolderLinkSettings {
    pub deletion_policy: covalent_protocol::FolderLinkPolicy,
    pub paused: bool,
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkSettingsState {
    pub revision: u64,
    pub settings: FolderLinkSettings,
    pub change_id: Uuid,
    pub changed_by: DeviceId,
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
        self.reconcile_trust()?;
        let source_id = self.link_source(folder_id)?;
        let state = self
            .snapshot
            .link_settings
            .get(&folder_id)
            .ok_or(SharingError::InvalidState)?;
        if change_id.is_nil() || !state.confirmed {
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
            self.commit_link_request(&request)?;
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
        self.reconcile_trust()?;
        if request.source_id != self.engine.device_id()
            || request.change_id.is_nil()
            || self.link_source(request.folder_id)? != self.engine.device_id()
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
            self.commit_link_request(request)?;
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

    fn commit_link_request(&mut self, request: &LinkSettingsRequest) -> Result<(), SharingError> {
        let mut next = self.snapshot.clone();
        let state = next
            .link_settings
            .get_mut(&request.folder_id)
            .ok_or(SharingError::InvalidState)?;
        if state.revision != request.expected_revision {
            return Err(SharingError::SettingsConflict);
        }
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or(SharingError::LimitExceeded)?;
        state.settings = request.settings;
        state.change_id = request.change_id;
        state.changed_by = request.requester_id;
        state.pending_change = None;
        state.conflicted_change = None;
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
        if commit.revision == current.revision
            && (commit.settings != current.settings
                || commit.change_id != current.change_id
                || commit.changed_by != current.changed_by)
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
        self.persist(next)
    }

    fn link_source(&self, folder_id: Uuid) -> Result<DeviceId, SharingError> {
        self.snapshot
            .shares
            .iter()
            .find(|s| !s.removed && s.offer.folder_id == folder_id && s.offer.link_policy.is_some())
            .map(|s| s.offer.source_device_id)
            .ok_or(SharingError::InvalidRecord)
    }

    pub(super) fn link_allows_transfer(&self, share: &Share) -> bool {
        share.offer.link_policy.is_none()
            || self
                .snapshot
                .link_settings
                .get(&share.offer.folder_id)
                .is_some_and(|state| state.confirmed && !state.settings.paused)
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
                    },
                    change_id: Uuid::nil(),
                    changed_by: share.offer.source_device_id,
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
