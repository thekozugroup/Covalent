//! Source-authenticated presentation metadata; never a peer grant or folder capability.

use super::*;
use crate::display_names::DisplayNames;

const MAX_ENDPOINTS: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RosterEndpoint {
    pub device_id: DeviceId,
    pub display_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointRoster {
    pub revision: u64,
    pub label: String,
    pub source: RosterEndpoint,
    pub destinations: Vec<RosterEndpoint>,
}

/// Authenticated by the existing folder-control envelope and tied to one
/// retained invitation. Siblings have identifiers and names only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointRosterCommit {
    pub source_id: DeviceId,
    pub target_id: DeviceId,
    pub folder_id: Uuid,
    pub offer_id: Uuid,
    pub roster: EndpointRoster,
}

fn valid_name(name: &str, maximum: usize) -> bool {
    !name.trim().is_empty() && name.len() <= maximum && !name.chars().any(char::is_control)
}

impl EndpointRoster {
    pub(crate) fn apply_display_names(&mut self, folder_id: Uuid, names: &DisplayNames) {
        if let Some(name) = names.folders.get(&folder_id) {
            self.label.clone_from(name);
        }
        for endpoint in std::iter::once(&mut self.source).chain(&mut self.destinations) {
            if let Some(name) = names.peers.get(&endpoint.device_id) {
                endpoint.display_name.clone_from(name);
            }
        }
    }

    fn validate(&self) -> Result<(), SharingError> {
        let nil = DeviceId::from_uuid(Uuid::nil());
        let mut ids = BTreeSet::from([self.source.device_id]);
        if self.revision == 0
            || !valid_name(&self.label, 256)
            || self.source.device_id == nil
            || !valid_name(&self.source.display_name, 80)
            || self.destinations.is_empty()
            || self.destinations.len() > MAX_ENDPOINTS
            || self.destinations.iter().any(|endpoint| {
                endpoint.device_id == nil
                    || !ids.insert(endpoint.device_id)
                    || !valid_name(&endpoint.display_name, 80)
            })
        {
            return Err(SharingError::InvalidRecord);
        }
        Ok(())
    }
}

impl FolderSharingJournal {
    /// Recompute only source-owned rosters from retained membership and local
    /// names. No receiver-supplied roster enters this derivation.
    pub(crate) fn endpoint_roster_records(
        &mut self,
        names: &DisplayNames,
    ) -> Result<Vec<FolderShareDelivery>, SharingError> {
        self.store
            .payload()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        let owner = self.engine.device_id();
        let config = self
            .engine
            .config()
            .map_err(|_| SharingError::InvalidState)?;
        let mut next = self.snapshot.clone();
        let mut changed = false;
        let mut folders = BTreeMap::<Uuid, Vec<&Share>>::new();
        for share in self.snapshot.shares.iter().filter(|s| {
            !s.removed && s.offer.source_device_id == owner && s.offer.link_policy.is_some()
        }) {
            // A presentation read must not revive revoked consent.
            if observe_trust(&config, share.peer_identity.device_id)?
                .is_some_and(|trust| trust.matches(share))
            {
                folders
                    .entry(share.offer.folder_id)
                    .or_default()
                    .push(share);
            }
        }
        let mut deliveries = Vec::new();
        for (folder_id, shares) in folders {
            let mut destinations = shares
                .iter()
                .map(|share| {
                    let id = share.peer_identity.device_id;
                    RosterEndpoint {
                        device_id: id,
                        display_name: names
                            .peers
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| config.trusted_peers[&id].display_name.clone()),
                    }
                })
                .collect::<Vec<_>>();
            destinations.sort_by_key(|endpoint| endpoint.device_id);
            let current = self.snapshot.endpoint_rosters.get(&folder_id);
            let mut roster = EndpointRoster {
                revision: current.map_or(1, |roster| roster.revision),
                label: names
                    .folders
                    .get(&folder_id)
                    .cloned()
                    .unwrap_or_else(|| shares[0].offer.label.clone()),
                source: RosterEndpoint {
                    device_id: owner,
                    display_name: config.device_name.clone(),
                },
                destinations,
            };
            roster.validate()?;
            if current != Some(&roster) {
                if let Some(current) = current {
                    roster.revision = current
                        .revision
                        .checked_add(1)
                        .ok_or(SharingError::LimitExceeded)?;
                }
                next.endpoint_rosters.insert(folder_id, roster.clone());
                changed = true;
            }
            for share in shares {
                deliveries.push(FolderShareDelivery {
                    peer_transport: share.peer_transport.clone(),
                    record: FolderShareRecord::EndpointRoster(EndpointRosterCommit {
                        source_id: owner,
                        target_id: share.offer.target_device_id,
                        folder_id,
                        offer_id: share.offer.offer_id,
                        roster: roster.clone(),
                    }),
                });
            }
        }
        if changed {
            self.persist(next)?;
        }
        Ok(deliveries)
    }

    /// Caller must authenticate source_id through the signed control envelope.
    /// This stores display data only and cannot add peers, consent or settings.
    pub fn receive_endpoint_roster(
        &mut self,
        commit: &EndpointRosterCommit,
    ) -> Result<(), SharingError> {
        commit.roster.validate()?;
        let owner = self.engine.device_id();
        if commit.target_id != owner
            || commit.source_id == owner
            || commit.roster.source.device_id != commit.source_id
            || !commit
                .roster
                .destinations
                .iter()
                .any(|endpoint| endpoint.device_id == owner)
        {
            return Err(SharingError::InvalidRecord);
        }
        let trust = self.trusted_peer(commit.source_id)?;
        let _share = self
            .snapshot
            .shares
            .iter()
            .find(|share| {
                !share.removed
                    && share.offer.offer_id == commit.offer_id
                    && share.offer.folder_id == commit.folder_id
                    && share.offer.source_device_id == commit.source_id
                    && share.offer.target_device_id == owner
                    && share.offer.link_policy.is_some()
                    && trust.matches(share)
            })
            .ok_or(SharingError::UntrustedPeer)?;
        if let Some(current) = self.snapshot.endpoint_rosters.get(&commit.folder_id) {
            if current == &commit.roster {
                return Ok(());
            }
            if commit.roster.revision <= current.revision {
                return Err(SharingError::InvalidRecord);
            }
        }
        let mut next = self.snapshot.clone();
        next.endpoint_rosters
            .insert(commit.folder_id, commit.roster.clone());
        self.persist(next)
    }
}

pub(super) fn reconcile(snapshot: &mut Snapshot) {
    snapshot.endpoint_rosters.retain(|folder, roster| {
        snapshot.shares.iter().any(|share| {
            !share.removed
                && share.offer.folder_id == *folder
                && share.offer.source_device_id == roster.source.device_id
                && share.offer.link_policy.is_some()
        })
    });
}

pub(super) fn validate(snapshot: &Snapshot) -> Result<(), SharingError> {
    if snapshot.endpoint_rosters.len() > MAX_SHARES {
        return Err(SharingError::LimitExceeded);
    }
    for (folder, roster) in &snapshot.endpoint_rosters {
        roster.validate()?;
        if !snapshot.shares.iter().any(|share| {
            !share.removed
                && share.offer.folder_id == *folder
                && share.offer.source_device_id == roster.source.device_id
                && share.offer.link_policy.is_some()
        }) {
            return Err(SharingError::InvalidState);
        }
    }
    Ok(())
}
