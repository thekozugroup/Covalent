//! Durable two-party folder consent, separate from worker lifecycle.
//!
//! All mutations require the owning node's serialized coordinator. Stop/reap
//! the current worker before replacing its desired configuration, and obtain
//! fresh settings through this journal before every launch. Peer revocation
//! must first stop the live worker; replay reconciliation here is the startup
//! barrier, not a way to recall bytes already received by another device.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::SocketAddr;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use covalent_core::{
    Engine, KeyProtector, NodeConfig, PublicIdentity, verify_folder_share_acceptance,
    verify_folder_share_commit, verify_folder_share_offer, verify_fresh_folder_share_offer,
};
use covalent_protocol::{
    DeviceId, FolderShareAcceptance, FolderShareCommit, FolderShareOffer, SyncEngineBinding,
    TransportBinding,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::config::{EngineDeviceId, EngineFolderConfig, EnginePeerConfig};
use super::{EngineInstallation, EngineSessionSettings, EngineStateStore};

const MAX_SHARES: usize = 512;
const MAX_RETAINED_OFFERS: usize = 4096;
const MAX_PENDING_OFFERS: usize = 128;
const MAX_PEER_PENDING_OFFERS: usize = 16;
const MAX_RENEWALS_PER_SHARE: usize = 128;
const MAX_REMOVAL_OFFER_IDS: usize = MAX_RENEWALS_PER_SHARE + 1;
const MAX_PENDING_REMOVALS: usize = MAX_SHARES;
const INVITATION_LIFETIME_MS: u64 = covalent_core::MAX_FOLDER_SHARE_LIFETIME_MS;

/// A local sharing decision failure, without keys, paths, or peer-supplied text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharingError {
    InvalidState,
    UntrustedPeer,
    InvalidRecord,
    Removed,
    AlreadyShared,
    FolderUnavailable,
    LimitExceeded,
    PersistenceUncertain,
}

impl fmt::Display for SharingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidState => "folder sharing state is invalid",
            Self::UntrustedPeer => "this device is no longer trusted for folder sharing",
            Self::InvalidRecord => "folder invitation could not be verified",
            Self::Removed => "this folder invitation was removed",
            Self::AlreadyShared => "this folder already has an invitation for this device",
            Self::FolderUnavailable => "the selected folder needs access again",
            Self::LimitExceeded => "folder sharing reached its local limit",
            Self::PersistenceUncertain => "folder sharing needs state recovery",
        })
    }
}
impl std::error::Error for SharingError {}

/// Consent state, not an assertion that files are up to date or a worker runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SharingPhase {
    Offered,
    AwaitingCommit,
    Ready,
    Paused,
    Removed,
}

/// Secret-free native UI summary. Local filesystem paths and engine IDs stay
/// inside this boundary; native folder capabilities are tracked separately.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareSummary {
    pub offer_id: Uuid,
    /// Older invitation identifiers replaced by this authenticated journal
    /// row. Clients can retire stale local grant bindings without inferring a
    /// relationship from user-visible labels or folder identifiers.
    pub superseded_offer_ids: Vec<Uuid>,
    pub folder_id: Uuid,
    pub label: String,
    pub peer_id: DeviceId,
    pub incoming: bool,
    pub phase: SharingPhase,
    /// Only unaccepted invitations expire. A durable acceptance remains valid
    /// for replay after its original delivery window has elapsed.
    pub expires_at_unix_ms: Option<u64>,
    /// This node has stopped locally and retained an authenticated notice for
    /// delivery. It does not claim that the peer has received the notice yet.
    pub remote_removal_pending: bool,
}

/// A withdrawal carried only inside the authenticated folder-control envelope.
///
/// The ordered offer chain lets a peer that missed one or more invitation
/// renewals match its exact retained prefix. This value has no signature of its
/// own and must never be accepted outside that verified envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FolderRemovalNotice {
    pub requester_id: DeviceId,
    pub target_id: DeviceId,
    pub offer_source_id: DeviceId,
    pub folder_id: Uuid,
    pub offer_ids: Vec<Uuid>,
}

/// One already durable record awaiting an idempotent authenticated delivery.
/// Engine management credentials and local folder capabilities are excluded.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum FolderShareRecord {
    Offer(FolderShareOffer),
    Acceptance {
        offer_id: Uuid,
        acceptance: FolderShareAcceptance,
    },
    Commit {
        offer_id: Uuid,
        commit: FolderShareCommit,
    },
    Removal(FolderRemovalNotice),
}

#[derive(Clone)]
pub struct FolderShareDelivery {
    pub peer_transport: TransportBinding,
    pub record: FolderShareRecord,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalRoot {
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Share {
    peer_identity: PublicIdentity,
    peer_transport: TransportBinding,
    peer_confirmed_at_unix_ms: u64,
    offer: FolderShareOffer,
    acceptance: Option<FolderShareAcceptance>,
    commit: Option<FolderShareCommit>,
    root: Option<LocalRoot>,
    paused: bool,
    removed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    superseded_offers: Vec<SupersededOffer>,
}

/// Minimal durable refusal metadata for offers replaced by an explicit renewal.
/// The old signed record and local path are deliberately not duplicated.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SupersededOffer {
    offer_id: Uuid,
    issued_at_unix_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    schema_version: u16,
    owner: PublicIdentity,
    binding: SyncEngineBinding,
    listener: SocketAddr,
    shares: Vec<Share>,
    tombstones: Vec<Tombstone>,
    freshness_floor_unix_ms: u64,
    #[serde(default)]
    pending_root_reset: Option<PendingRootReset>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_remote_removals: Vec<PendingRemoteRemoval>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    remote_removal_tombstones: Vec<RemoteRemovalTombstone>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingRootReset {
    offer_id: Uuid,
    folder_id: Uuid,
    replacement: LocalRoot,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingRemoteRemoval {
    peer_identity: PublicIdentity,
    peer_transport: TransportBinding,
    peer_confirmed_at_unix_ms: u64,
    offer_source_id: DeviceId,
    folder_id: Uuid,
    /// Chronological superseded identifiers followed by the current ID.
    offer_ids: Vec<Uuid>,
}

/// Path-free refusal history for an authenticated removal that may overtake
/// an invitation or one of its renewals. It carries no peer transport pin and
/// grants no authority; its chain prevents the same logical invitation from
/// being revived by a delayed descendant renewal.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoteRemovalTombstone {
    peer_id: DeviceId,
    offer_source_id: DeviceId,
    folder_id: Uuid,
    offer_ids: Vec<Uuid>,
}

/// One instance- and revision-bound root selection prepared before a worker is
/// stopped. Its path and inode remain private to the journal.
pub(crate) struct PreparedRootRepair {
    journal: Arc<()>,
    expected_revision: u64,
    offer_id: Uuid,
    folder_id: Uuid,
    replacement: LocalRoot,
    changes_root: bool,
}

impl fmt::Debug for PreparedRootRepair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedRootRepair([PRIVATE])")
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Tombstone {
    offer_id: Uuid,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    superseded_offer_ids: Vec<Uuid>,
    folder_id: Uuid,
    label: String,
    peer_id: DeviceId,
    incoming: bool,
    issued_at_unix_ms: u64,
}

/// One serialized local consent journal. Signatures are returned only after
/// their associated local decision is durable. Removed offers are tombstones;
/// pairing the same peer again does not restore any old folder permission.
pub struct FolderSharingJournal {
    engine: Arc<Engine>,
    installation: Arc<EngineInstallation>,
    store: EngineStateStore,
    snapshot: Snapshot,
    preparation_identity: Arc<()>,
}

impl fmt::Debug for FolderSharingJournal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FolderSharingJournal([PRIVATE])")
    }
}

impl FolderSharingJournal {
    /// Initialize a new installation with a concrete advertised engine endpoint
    /// and its actual listener. A wildcard listener may advertise a concrete IP
    /// at the same port. The node host owns interface selection/reachability.
    pub fn create(
        engine: Arc<Engine>,
        installation: Arc<EngineInstallation>,
        protector: Arc<dyn KeyProtector>,
        listener: SocketAddr,
        advertised: SocketAddr,
    ) -> Result<Self, SharingError> {
        let binding = SyncEngineBinding::new(
            engine.device_id(),
            installation.device_id().as_str(),
            &advertised.to_string(),
        )
        .map_err(|_| SharingError::InvalidState)?;
        let snapshot = Snapshot {
            schema_version: 1,
            owner: engine.public_identity(),
            binding,
            listener,
            shares: Vec::new(),
            tombstones: Vec::new(),
            freshness_floor_unix_ms: 0,
            pending_root_reset: None,
            pending_remote_removals: Vec::new(),
            remote_removal_tombstones: Vec::new(),
        };
        validate_snapshot(&snapshot, &engine, &installation)?;
        let payload = serde_json::to_vec(&snapshot).map_err(|_| SharingError::InvalidState)?;
        let store = EngineStateStore::create(Arc::clone(&installation), protector, &payload)
            .map_err(|_| SharingError::PersistenceUncertain)?;
        Ok(Self {
            engine,
            installation,
            store,
            snapshot,
            preparation_identity: Arc::new(()),
        })
    }

    /// Authenticate existing state and durably remove every share whose peer
    /// is revoked, missing, or bound to a different Covalent key before launch.
    pub fn open(
        engine: Arc<Engine>,
        installation: Arc<EngineInstallation>,
        protector: Arc<dyn KeyProtector>,
    ) -> Result<Self, SharingError> {
        let store = EngineStateStore::open(Arc::clone(&installation), protector)
            .map_err(|_| SharingError::InvalidState)?;
        let snapshot =
            serde_json::from_slice(store.payload().map_err(|_| SharingError::InvalidState)?)
                .map_err(|_| SharingError::InvalidState)?;
        validate_snapshot(&snapshot, &engine, &installation)?;
        let mut journal = Self {
            engine,
            installation,
            store,
            snapshot,
            preparation_identity: Arc::new(()),
        };
        journal.reconcile_trust()?;
        Ok(journal)
    }

    /// The verified local public engine binding sent only over Covalent's
    /// authenticated pairing/control transport, never entered by the user.
    pub fn binding(&self) -> &SyncEngineBinding {
        &self.snapshot.binding
    }

    /// The exact listener retained with this installation's signed address.
    pub fn listener(&self) -> SocketAddr {
        self.snapshot.listener
    }

    /// Last durably committed local revision. The service uses it only to
    /// classify mutation failures; it is not a worker-launch authorization.
    pub fn revision(&self) -> u64 {
        self.store.revision()
    }

    /// Reconstruct delivery work after a process or network interruption. A
    /// sender may cache acknowledgements in memory; after cold restart it
    /// repeats these exact durable records, never generates a new acceptance.
    /// A completed recipient has no further record to send. The source retains
    /// its commit so loss of the final response cannot strand the recipient.
    pub fn outbound_records(&mut self) -> Result<Vec<FolderShareDelivery>, SharingError> {
        self.reconcile_trust()?;
        let config = self
            .engine
            .config()
            .map_err(|_| SharingError::InvalidState)?;
        let mut result = Vec::new();
        // Withdrawals take precedence over invitations and commits for the
        // same peer. Delivery admits one record per peer per cadence, so this
        // avoids a permanently rejected invitation starving a local removal.
        for pending in &self.snapshot.pending_remote_removals {
            let trust = observe_trust(&config, pending.peer_identity.device_id)?
                .ok_or(SharingError::UntrustedPeer)?;
            if trust.identity != pending.peer_identity
                || trust.transport != pending.peer_transport
                || trust.confirmed_at_unix_ms != pending.peer_confirmed_at_unix_ms
            {
                return Err(SharingError::UntrustedPeer);
            }
            result.push(FolderShareDelivery {
                peer_transport: trust.transport,
                record: FolderShareRecord::Removal(FolderRemovalNotice {
                    requester_id: self.engine.device_id(),
                    target_id: pending.peer_identity.device_id,
                    offer_source_id: pending.offer_source_id,
                    folder_id: pending.folder_id,
                    offer_ids: pending.offer_ids.clone(),
                }),
            });
        }
        for share in &self.snapshot.shares {
            if share.removed {
                continue;
            }
            let trust = observe_trust(&config, share.peer_identity.device_id)?
                .ok_or(SharingError::UntrustedPeer)?;
            if !trust.matches(share) {
                return Err(SharingError::UntrustedPeer);
            }
            let record = if share.offer.source_device_id == self.engine.device_id() {
                if let Some(commit) = &share.commit {
                    FolderShareRecord::Commit {
                        offer_id: share.offer.offer_id,
                        commit: commit.clone(),
                    }
                } else {
                    FolderShareRecord::Offer(share.offer.clone())
                }
            } else if share.commit.is_none() {
                let Some(acceptance) = &share.acceptance else {
                    continue;
                };
                FolderShareRecord::Acceptance {
                    offer_id: share.offer.offer_id,
                    acceptance: acceptance.clone(),
                }
            } else {
                continue;
            };
            result.push(FolderShareDelivery {
                peer_transport: trust.transport,
                record,
            });
        }
        Ok(result)
    }

    /// List local consent states after revalidating the durable revision.
    pub fn summaries(&self) -> Result<Vec<ShareSummary>, SharingError> {
        self.store
            .payload()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        let mut summaries: Vec<_> = self
            .snapshot
            .shares
            .iter()
            .map(|share| ShareSummary {
                offer_id: share.offer.offer_id,
                superseded_offer_ids: share
                    .superseded_offers
                    .iter()
                    .map(|superseded| superseded.offer_id)
                    .collect(),
                folder_id: share.offer.folder_id,
                label: share.offer.label.clone(),
                peer_id: share.peer_identity.device_id,
                incoming: share.offer.target_device_id == self.engine.device_id(),
                expires_at_unix_ms: (!share.removed && share.acceptance.is_none())
                    .then_some(share.offer.expires_at_unix_ms),
                phase: if share.removed {
                    SharingPhase::Removed
                } else if share.paused {
                    SharingPhase::Paused
                } else if share.commit.is_some() {
                    SharingPhase::Ready
                } else if share.acceptance.is_some() {
                    SharingPhase::AwaitingCommit
                } else {
                    SharingPhase::Offered
                },
                remote_removal_pending: false,
            })
            .collect();
        let superseded_removed_ids = self
            .snapshot
            .tombstones
            .iter()
            .flat_map(|removed| removed.superseded_offer_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        summaries.extend(
            self.snapshot
                .tombstones
                .iter()
                .filter(|removed| !superseded_removed_ids.contains(&removed.offer_id))
                .map(|removed| ShareSummary {
                    offer_id: removed.offer_id,
                    superseded_offer_ids: removed.superseded_offer_ids.clone(),
                    folder_id: removed.folder_id,
                    label: removed.label.clone(),
                    peer_id: removed.peer_id,
                    incoming: removed.incoming,
                    phase: SharingPhase::Removed,
                    expires_at_unix_ms: None,
                    remote_removal_pending: self.snapshot.pending_remote_removals.iter().any(
                        |pending| {
                            pending.peer_identity.device_id == removed.peer_id
                                && pending.folder_id == removed.folder_id
                                && pending.offer_ids.last() == Some(&removed.offer_id)
                        },
                    ),
                }),
        );
        Ok(summaries)
    }

    /// Reconcile current Covalent trust before an offline, worker-free status
    /// view. Root availability is deliberately not required merely to list or
    /// remove consent after native capability loss.
    pub(crate) fn reconciled_summaries(&mut self) -> Result<Vec<ShareSummary>, SharingError> {
        self.reconcile_trust()?;
        self.summaries()
    }

    /// Map each active engine peer identity back to the authenticated Covalent
    /// peer that supplied it. Pending, paused and removed shares have no live
    /// engine membership and are deliberately absent.
    pub(crate) fn active_engine_peers(
        &self,
    ) -> Result<BTreeMap<EngineDeviceId, DeviceId>, SharingError> {
        self.store
            .payload()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        let mut result = BTreeMap::new();
        for share in self
            .snapshot
            .shares
            .iter()
            .filter(|share| !share.removed && !share.paused && share.commit.is_some())
        {
            let binding = if share.offer.source_device_id == self.engine.device_id() {
                &share
                    .acceptance
                    .as_ref()
                    .ok_or(SharingError::InvalidState)?
                    .target_engine
            } else {
                &share.offer.source_engine
            };
            let engine_id = EngineDeviceId::parse(binding.engine_device_id.as_str())
                .map_err(|_| SharingError::InvalidRecord)?;
            if let Some(previous) = result.insert(engine_id, share.peer_identity.device_id)
                && previous != share.peer_identity.device_id
            {
                return Err(SharingError::InvalidRecord);
            }
        }
        Ok(result)
    }

    /// Retain the source user's selected folder before returning a signed offer.
    pub fn offer(
        &mut self,
        peer_id: DeviceId,
        folder_id: Uuid,
        label: &str,
        selected_root: &Path,
        now: u64,
    ) -> Result<FolderShareOffer, SharingError> {
        self.reconcile_trust()?;
        let peer = self.trusted_peer(peer_id)?;
        let root = capture_root(selected_root, &self.installation)?;
        if let Some(existing) = self.snapshot.shares.iter().find(|share| {
            !share.removed
                && share.offer.folder_id == folder_id
                && share.peer_identity.device_id == peer_id
        }) {
            if existing.offer.source_device_id == self.engine.device_id()
                && existing.offer.label == label
                && existing
                    .root
                    .as_ref()
                    .is_some_and(|old| same_root(old, &root))
                && peer.matches(existing)
            {
                return Ok(existing.offer.clone());
            }
            return Err(SharingError::AlreadyShared);
        }
        self.advance_freshness_floor(now)?;
        if now < self.snapshot.freshness_floor_unix_ms {
            return Err(SharingError::InvalidRecord);
        }
        self.check_capacity()?;
        self.check_not_shared(folder_id, peer_id)?;
        let offer = self
            .engine
            .issue_folder_share_offer(
                peer_id,
                folder_id,
                label,
                self.snapshot.binding.clone(),
                None,
                now,
                INVITATION_LIFETIME_MS,
            )
            .map_err(|_| SharingError::InvalidRecord)?;
        let mut next = self.snapshot.clone();
        next.shares.push(Share {
            peer_identity: peer.identity,
            peer_transport: peer.transport,
            peer_confirmed_at_unix_ms: peer.confirmed_at_unix_ms,
            offer: offer.clone(),
            acceptance: None,
            commit: None,
            root: Some(root),
            paused: false,
            removed: false,
            superseded_offers: Vec::new(),
        });
        self.persist(next)?;
        Ok(offer)
    }

    /// Retain a fresh verified invitation without any local filesystem grant.
    /// Exact retransmission is idempotent, including after the original expiry;
    /// a removed ID or another signed record using that ID is never revived.
    pub fn receive_offer(&mut self, offer: FolderShareOffer, now: u64) -> Result<(), SharingError> {
        self.reconcile_trust()?;
        let peer = self.trusted_peer(offer.source_device_id)?;
        verify_folder_share_offer(&offer, &peer.identity, self.engine.device_id())
            .map_err(|_| SharingError::InvalidRecord)?;
        if self
            .snapshot
            .remote_removal_tombstones
            .iter()
            .any(|removed| {
                removed.peer_id == offer.source_device_id
                    && removed.offer_source_id == offer.source_device_id
                    && removed.folder_id == offer.folder_id
            })
        {
            // A refusal applies to the authenticated logical invitation, not
            // only the IDs the recipient had observed. A renewal that crossed
            // the removal in flight cannot recreate the removed choice.
            return Err(SharingError::Removed);
        }
        if self.snapshot.tombstones.iter().any(|removed| {
            removed.offer_id == offer.offer_id
                || removed.superseded_offer_ids.contains(&offer.offer_id)
        }) {
            return Err(SharingError::Removed);
        }
        if let Some(old) = self
            .snapshot
            .shares
            .iter()
            .find(|s| s.offer.offer_id == offer.offer_id)
        {
            if old.removed {
                return Err(SharingError::Removed);
            }
            if old.offer == offer {
                return Ok(());
            }
            return Err(SharingError::InvalidRecord);
        }
        if self.snapshot.shares.iter().any(|share| {
            share
                .superseded_offers
                .iter()
                .any(|superseded| superseded.offer_id == offer.offer_id)
        }) {
            return Err(SharingError::Removed);
        }
        verify_fresh_folder_share_offer(&offer, &peer.identity, self.engine.device_id(), now)
            .map_err(|_| SharingError::InvalidRecord)?;
        if self.snapshot.tombstones.iter().any(|removed| {
            removed.folder_id == offer.folder_id
                && removed.peer_id == peer.identity.device_id
                && removed.incoming
        }) {
            return Err(SharingError::Removed);
        }
        self.advance_freshness_floor(now)?;
        if offer.issued_at_unix_ms < self.snapshot.freshness_floor_unix_ms {
            return Err(SharingError::Removed);
        }
        if let Some(index) = self.snapshot.shares.iter().position(|share| {
            !share.removed
                && share.offer.folder_id == offer.folder_id
                && share.peer_identity.device_id == peer.identity.device_id
        }) {
            let previous = &self.snapshot.shares[index];
            if previous.offer.target_device_id != self.engine.device_id()
                || previous.commit.is_some()
                || now < previous.offer.expires_at_unix_ms
                || offer.issued_at_unix_ms <= previous.offer.issued_at_unix_ms
                || offer.expires_at_unix_ms <= previous.offer.expires_at_unix_ms
                || offer.label != previous.offer.label
                || offer.source_engine != previous.offer.source_engine
                || offer.pairing_id != previous.offer.pairing_id
                || !peer.matches(previous)
            {
                return Err(SharingError::AlreadyShared);
            }
            self.check_renewal_capacity(previous)?;
            let mut next = self.snapshot.clone();
            let replacement = &mut next.shares[index];
            replacement.superseded_offers.push(SupersededOffer {
                offer_id: replacement.offer.offer_id,
                issued_at_unix_ms: replacement.offer.issued_at_unix_ms,
            });
            replacement.offer = offer;
            // A target acceptance that expired before reaching the source can
            // never be committed. Do not transfer that consent to the fresh
            // invitation: the recipient must choose a destination and sign it
            // again. Keeping the share row only preserves refusal history.
            replacement.acceptance = None;
            replacement.commit = None;
            replacement.root = None;
            replacement.paused = false;
            self.persist(next)?;
            return Ok(());
        }
        let pending = self
            .snapshot
            .shares
            .iter()
            .filter(|s| {
                s.offer.target_device_id == self.engine.device_id() && s.acceptance.is_none()
            })
            .collect::<Vec<_>>();
        if pending.len() >= MAX_PENDING_OFFERS
            || pending
                .iter()
                .filter(|s| s.peer_identity.device_id == peer.identity.device_id)
                .count()
                >= MAX_PEER_PENDING_OFFERS
        {
            return Err(SharingError::LimitExceeded);
        }
        self.check_capacity()?;
        self.check_not_shared(offer.folder_id, peer.identity.device_id)?;
        let mut next = self.snapshot.clone();
        next.shares.push(Share {
            peer_identity: peer.identity,
            peer_transport: peer.transport,
            peer_confirmed_at_unix_ms: peer.confirmed_at_unix_ms,
            offer,
            acceptance: None,
            commit: None,
            root: None,
            paused: false,
            removed: false,
            superseded_offers: Vec::new(),
        });
        self.persist(next)
    }

    /// Replace one expired outgoing invitation with a newly signed invitation.
    ///
    /// This is an explicit local action. It never changes an accepted or removed
    /// consent record. Retrying with a superseded identifier returns the current
    /// durable replacement without signing or persisting another offer.
    pub fn renew_offer(
        &mut self,
        offer_id: Uuid,
        now: u64,
    ) -> Result<FolderShareOffer, SharingError> {
        self.reconcile_trust()?;
        if self.snapshot.tombstones.iter().any(|removed| {
            removed.offer_id == offer_id || removed.superseded_offer_ids.contains(&offer_id)
        }) {
            return Err(SharingError::Removed);
        }
        if let Some(share) = self.snapshot.shares.iter().find(|share| {
            share
                .superseded_offers
                .iter()
                .any(|superseded| superseded.offer_id == offer_id)
        }) {
            return if share.offer.source_device_id == self.engine.device_id() {
                Ok(share.offer.clone())
            } else {
                Err(SharingError::InvalidRecord)
            };
        }
        let index = self.index(offer_id)?;
        let share = &self.snapshot.shares[index];
        if share.offer.source_device_id != self.engine.device_id()
            || share.acceptance.is_some()
            || share.commit.is_some()
            || now < share.offer.expires_at_unix_ms
        {
            return Err(SharingError::InvalidRecord);
        }
        let peer = self.trusted_peer(share.peer_identity.device_id)?;
        if !peer.matches(share) {
            return Err(SharingError::UntrustedPeer);
        }
        let root = share.root.as_ref().ok_or(SharingError::InvalidState)?;
        validate_current_root(root)?;
        self.check_renewal_capacity(share)?;
        if now < self.snapshot.freshness_floor_unix_ms {
            return Err(SharingError::InvalidRecord);
        }
        let replacement = self
            .engine
            .issue_folder_share_offer(
                share.peer_identity.device_id,
                share.offer.folder_id,
                &share.offer.label,
                self.snapshot.binding.clone(),
                share.offer.pairing_id.as_deref(),
                now,
                INVITATION_LIFETIME_MS,
            )
            .map_err(|_| SharingError::InvalidRecord)?;
        if replacement.issued_at_unix_ms <= share.offer.issued_at_unix_ms
            || replacement.expires_at_unix_ms <= share.offer.expires_at_unix_ms
        {
            return Err(SharingError::InvalidRecord);
        }
        let mut next = self.snapshot.clone();
        advance_freshness_floor_candidate(&mut next, now);
        let renewed = &mut next.shares[index];
        renewed.superseded_offers.push(SupersededOffer {
            offer_id: renewed.offer.offer_id,
            issued_at_unix_ms: renewed.offer.issued_at_unix_ms,
        });
        renewed.offer = replacement.clone();
        self.persist(next)?;
        Ok(replacement)
    }

    /// One user confirmation chooses the destination and accepts. The signature
    /// is never returned if persisting that local selection did not succeed.
    pub fn accept(
        &mut self,
        offer_id: Uuid,
        selected_root: &Path,
        now: u64,
    ) -> Result<FolderShareAcceptance, SharingError> {
        self.reconcile_trust()?;
        let index = self.index(offer_id)?;
        let share = &self.snapshot.shares[index];
        if share.offer.target_device_id != self.engine.device_id() {
            return Err(SharingError::InvalidRecord);
        }
        let root = capture_root(selected_root, &self.installation)?;
        if let Some(acceptance) = &share.acceptance {
            if share.root.as_ref().is_some_and(|r| same_root(r, &root)) {
                return Ok(acceptance.clone());
            }
            return Err(SharingError::AlreadyShared);
        }
        let acceptance = self
            .engine
            .accept_folder_share(&share.offer, self.snapshot.binding.clone(), now)
            .map_err(|_| SharingError::InvalidRecord)?;
        let mut next = self.snapshot.clone();
        next.shares[index].root = Some(root);
        next.shares[index].acceptance = Some(acceptance.clone());
        self.persist(next)?;
        Ok(acceptance)
    }

    /// The source commits both signatures durably before becoming eligible for
    /// sync. The returned commit can be retried after transport interruption.
    pub fn receive_acceptance(
        &mut self,
        offer_id: Uuid,
        acceptance: FolderShareAcceptance,
        now: u64,
    ) -> Result<FolderShareCommit, SharingError> {
        self.reconcile_trust()?;
        let index = self.index(offer_id)?;
        let share = &self.snapshot.shares[index];
        if share.offer.source_device_id != self.engine.device_id() {
            return Err(SharingError::InvalidRecord);
        }
        if let Some(old) = &share.acceptance {
            if old == &acceptance {
                return share.commit.clone().ok_or(SharingError::InvalidState);
            }
            return Err(SharingError::InvalidRecord);
        }
        let commit = self
            .engine
            .commit_folder_share_offer(&share.offer, &acceptance, now)
            .map_err(|_| SharingError::InvalidRecord)?;
        let mut next = self.snapshot.clone();
        next.shares[index].acceptance = Some(acceptance);
        next.shares[index].commit = Some(commit.clone());
        self.persist(next)?;
        Ok(commit)
    }

    /// The recipient becomes eligible only after the complete signed commit is
    /// durable. Eligibility still requires current peer trust and root access.
    pub fn receive_commit(
        &mut self,
        offer_id: Uuid,
        commit: FolderShareCommit,
    ) -> Result<(), SharingError> {
        self.reconcile_trust()?;
        let index = self.index(offer_id)?;
        let share = &self.snapshot.shares[index];
        if share.offer.target_device_id != self.engine.device_id() {
            return Err(SharingError::InvalidRecord);
        }
        if let Some(old) = &share.commit {
            return if old == &commit {
                Ok(())
            } else {
                Err(SharingError::InvalidRecord)
            };
        }
        let acceptance = share
            .acceptance
            .as_ref()
            .ok_or(SharingError::InvalidRecord)?;
        verify_folder_share_commit(
            &share.offer,
            acceptance,
            &commit,
            &share.peer_identity,
            &self.snapshot.owner,
        )
        .map_err(|_| SharingError::InvalidRecord)?;
        let mut next = self.snapshot.clone();
        next.shares[index].commit = Some(commit);
        self.persist(next)
    }

    /// Persist a local pause/resume decision. The owning lifecycle coordinator
    /// must reap the old worker before returning success to its API caller.
    pub fn set_paused(&mut self, offer_id: Uuid, paused: bool) -> Result<(), SharingError> {
        self.reconcile_trust()?;
        let index = self.index(offer_id)?;
        if self.snapshot.shares[index].paused == paused {
            return Ok(());
        }
        let mut next = self.snapshot.clone();
        next.shares[index].paused = paused;
        self.persist(next)
    }

    /// Tombstone one share while preserving all user files. Exact repeated
    /// removal succeeds; replayed offers/acceptances/commits remain refused.
    pub fn remove(&mut self, offer_id: Uuid) -> Result<(), SharingError> {
        self.store
            .payload()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        if self
            .snapshot
            .tombstones
            .iter()
            .any(|removed| removed.offer_id == offer_id)
        {
            return Ok(());
        }
        let index = self
            .snapshot
            .shares
            .iter()
            .position(|s| s.offer.offer_id == offer_id)
            .ok_or(SharingError::InvalidRecord)?;
        if self.snapshot.shares[index].removed {
            return Ok(());
        }
        let mut next = self.snapshot.clone();
        let share = &next.shares[index];
        let mut offer_ids = share
            .superseded_offers
            .iter()
            .map(|old| old.offer_id)
            .collect::<Vec<_>>();
        offer_ids.push(share.offer.offer_id);
        if offer_ids.len() > MAX_REMOVAL_OFFER_IDS
            || next.pending_remote_removals.len() >= MAX_PENDING_REMOVALS
        {
            return Err(SharingError::LimitExceeded);
        }
        next.pending_remote_removals.push(PendingRemoteRemoval {
            peer_identity: share.peer_identity.clone(),
            peer_transport: share.peer_transport.clone(),
            peer_confirmed_at_unix_ms: share.peer_confirmed_at_unix_ms,
            offer_source_id: share.offer.source_device_id,
            folder_id: share.offer.folder_id,
            offer_ids,
        });
        next.shares[index].removed = true;
        self.persist(next)
    }

    /// Apply a withdrawal only after the outer control request authenticated
    /// `notice.requester_id`. No notice is echoed back to its sender.
    pub fn receive_removal(&mut self, notice: &FolderRemovalNotice) -> Result<(), SharingError> {
        self.reconcile_trust()?;
        validate_removal_notice(notice, self.engine.device_id())?;
        self.trusted_peer(notice.requester_id)?;

        let matching_refusal = self
            .snapshot
            .remote_removal_tombstones
            .iter()
            .position(|removed| {
                removed.peer_id == notice.requester_id
                    && removed.offer_source_id == notice.offer_source_id
                    && removed.folder_id == notice.folder_id
            });
        if matching_refusal.is_some_and(|index| {
            self.snapshot.remote_removal_tombstones[index].offer_ids == notice.offer_ids
        }) {
            return Ok(());
        }
        if self
            .snapshot
            .remote_removal_tombstones
            .iter()
            .enumerate()
            .any(|(index, removed)| {
                Some(index) != matching_refusal
                    && removed
                        .offer_ids
                        .iter()
                        .any(|offer_id| notice.offer_ids.contains(offer_id))
            })
            || matching_refusal.is_some_and(|index| {
                let retained = &self.snapshot.remote_removal_tombstones[index].offer_ids;
                !is_prefix(retained, &notice.offer_ids) && !is_prefix(&notice.offer_ids, retained)
            })
            || notice.offer_ids.iter().any(|offer_id| {
                retained_offer_binding(&self.snapshot, *offer_id).is_some_and(|binding| {
                    binding
                        != (
                            notice.requester_id,
                            notice.folder_id,
                            notice.offer_source_id,
                        )
                })
            })
        {
            return Err(SharingError::InvalidRecord);
        }

        let matching_share = self.snapshot.shares.iter().position(|share| {
            share.peer_identity.device_id == notice.requester_id
                && share.offer.folder_id == notice.folder_id
        });
        let mut canonical_ids = notice.offer_ids.clone();
        if let Some(index) = matching_refusal {
            let retained = &self.snapshot.remote_removal_tombstones[index].offer_ids;
            if retained.len() > canonical_ids.len() {
                canonical_ids.clone_from(retained);
            }
        }
        if let Some(index) = matching_share {
            let share = &self.snapshot.shares[index];
            if share.offer.source_device_id != notice.offer_source_id {
                return Err(SharingError::InvalidRecord);
            }
            let mut known = share
                .superseded_offers
                .iter()
                .map(|old| old.offer_id)
                .collect::<Vec<_>>();
            known.push(share.offer.offer_id);
            // Removal and renewal can cross in flight. Either side may know a
            // longer prefix of the same authenticated logical invitation. A
            // shorter removal still stops known descendants; unrelated forks
            // remain invalid.
            if !is_prefix(&known, &canonical_ids) && !is_prefix(&canonical_ids, &known) {
                return Err(SharingError::InvalidRecord);
            }
            if known.len() > canonical_ids.len() {
                canonical_ids = known;
            }
        }
        if canonical_ids.len() > MAX_REMOVAL_OFFER_IDS
            || self
                .snapshot
                .remote_removal_tombstones
                .iter()
                .enumerate()
                .any(|(index, removed)| {
                    Some(index) != matching_refusal
                        && removed
                            .offer_ids
                            .iter()
                            .any(|offer_id| canonical_ids.contains(offer_id))
                })
        {
            return Err(SharingError::InvalidRecord);
        }
        if matching_refusal.is_some_and(|index| {
            self.snapshot.remote_removal_tombstones[index].offer_ids == canonical_ids
        }) && matching_share.is_none_or(|index| self.snapshot.shares[index].removed)
        {
            return Ok(());
        }

        // An authenticated withdrawal may overtake its invitation. Retain the
        // exact IDs even when none were known so delayed delivery cannot
        // recreate consent after this acknowledgement.
        let retained_ids = self.snapshot.remote_removal_tombstones.iter().try_fold(
            0_usize,
            |total, removed| {
                total
                    .checked_add(removed.offer_ids.len())
                    .ok_or(SharingError::LimitExceeded)
            },
        )?;
        let replaced_len = matching_refusal
            .map(|index| {
                self.snapshot.remote_removal_tombstones[index]
                    .offer_ids
                    .len()
            })
            .unwrap_or(0);
        if (matching_refusal.is_none()
            && self.snapshot.remote_removal_tombstones.len() >= MAX_RETAINED_OFFERS)
            || self
                .snapshot
                .remote_removal_tombstones
                .iter()
                .filter(|removed| removed.peer_id == notice.requester_id)
                .count()
                >= MAX_SHARES
                && matching_refusal.is_none()
            || retained_ids
                .checked_sub(replaced_len)
                .and_then(|total| total.checked_add(canonical_ids.len()))
                .is_none_or(|total| total > MAX_RETAINED_OFFERS)
        {
            return Err(SharingError::LimitExceeded);
        }
        let mut next = self.snapshot.clone();
        if let Some(index) = matching_share {
            next.shares[index].removed = true;
        }
        if let Some(index) = matching_refusal {
            next.remote_removal_tombstones[index].offer_ids = canonical_ids;
        } else {
            next.remote_removal_tombstones.push(RemoteRemovalTombstone {
                peer_id: notice.requester_id,
                offer_source_id: notice.offer_source_id,
                folder_id: notice.folder_id,
                offer_ids: canonical_ids,
            });
        }
        self.persist(next)
    }

    /// Stop replaying one withdrawal only after its peer returned an
    /// authenticated acknowledgement. Lost acknowledgements remain retryable.
    pub fn acknowledge_removal(
        &mut self,
        peer_id: DeviceId,
        current_offer_id: Uuid,
    ) -> Result<(), SharingError> {
        self.store
            .payload()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        let Some(index) = self
            .snapshot
            .pending_remote_removals
            .iter()
            .position(|pending| {
                pending.peer_identity.device_id == peer_id
                    && pending.offer_ids.last() == Some(&current_offer_id)
            })
        else {
            return if self
                .snapshot
                .tombstones
                .iter()
                .any(|removed| removed.peer_id == peer_id && removed.offer_id == current_offer_id)
            {
                Ok(())
            } else {
                Err(SharingError::InvalidRecord)
            };
        };
        let mut next = self.snapshot.clone();
        next.pending_remote_removals.remove(index);
        self.persist(next)
    }

    /// Validate and capture a replacement root before the lifecycle owner
    /// stops a healthy worker. The returned value is valid only for this exact
    /// journal instance and revision.
    pub(crate) fn prepare_root_repair(
        &mut self,
        offer_id: Uuid,
        selected_root: &Path,
    ) -> Result<PreparedRootRepair, SharingError> {
        self.reconcile_trust()?;
        let index = self.index(offer_id)?;
        if self.snapshot.shares[index].commit.is_none() {
            return Err(SharingError::InvalidState);
        }
        let old_root = self.snapshot.shares[index]
            .root
            .as_ref()
            .ok_or(SharingError::FolderUnavailable)?;
        let replacement = capture_root(selected_root, &self.installation)?;
        let folder_id = self.snapshot.shares[index].offer.folder_id;
        let changes_root = !same_root(old_root, &replacement);
        // Renewing filesystem capability for the exact retained inode does
        // not activate a paused share or change its engine configuration.
        // Moving a paused share still requires an explicit resume first so a
        // root-reset transition cannot be hidden behind administrative pause.
        if self.snapshot.shares[index].paused && changes_root {
            return Err(SharingError::InvalidState);
        }
        if let Some(pending) = &self.snapshot.pending_root_reset {
            if pending.offer_id != offer_id
                || pending.folder_id != folder_id
                || !same_root(&pending.replacement, &replacement)
                || changes_root
            {
                return Err(SharingError::InvalidState);
            }
        } else if changes_root
            && self
                .snapshot
                .shares
                .iter()
                .enumerate()
                .any(|(other, share)| {
                    other != index && !share.removed && share.offer.folder_id == folder_id
                })
        {
            return Err(SharingError::AlreadyShared);
        }
        Ok(PreparedRootRepair {
            journal: Arc::clone(&self.preparation_identity),
            expected_revision: self.store.revision(),
            offer_id,
            folder_id,
            replacement,
            changes_root,
        })
    }

    /// Persist a prepared root change and its per-folder index-reset intent.
    /// Replaying the exact pending selection is allocation-free and does not
    /// advance the durable revision.
    pub(crate) fn commit_root_repair(
        &mut self,
        prepared: PreparedRootRepair,
    ) -> Result<Option<Uuid>, SharingError> {
        if !Arc::ptr_eq(&prepared.journal, &self.preparation_identity)
            || prepared.expected_revision != self.store.revision()
        {
            return Err(SharingError::InvalidState);
        }
        let index = self.index(prepared.offer_id)?;
        if self.snapshot.shares[index].offer.folder_id != prepared.folder_id {
            return Err(SharingError::InvalidState);
        }
        let observed = capture_root(&prepared.replacement.path, &self.installation)?;
        if !same_root(&observed, &prepared.replacement) {
            return Err(SharingError::FolderUnavailable);
        }
        if let Some(pending) = &self.snapshot.pending_root_reset {
            if pending.offer_id != prepared.offer_id
                || pending.folder_id != prepared.folder_id
                || !same_root(&pending.replacement, &prepared.replacement)
                || self.snapshot.shares[index]
                    .root
                    .as_ref()
                    .is_none_or(|root| !same_root(root, &prepared.replacement))
            {
                return Err(SharingError::InvalidState);
            }
            return Ok(Some(pending.folder_id));
        }
        if !prepared.changes_root {
            return Ok(None);
        }
        let mut next = self.snapshot.clone();
        next.shares[index].root = Some(prepared.replacement.clone());
        next.pending_root_reset = Some(PendingRootReset {
            offer_id: prepared.offer_id,
            folder_id: prepared.folder_id,
            replacement: prepared.replacement,
        });
        self.persist(next)?;
        Ok(Some(prepared.folder_id))
    }

    /// Prepare and persist an offline repair. This method never launches a
    /// worker; the retained reset intent is completed only by the lifecycle
    /// coordinator after an authenticated engine reset and exact reap.
    pub fn repair_root(
        &mut self,
        offer_id: Uuid,
        selected_root: &Path,
    ) -> Result<(), SharingError> {
        let prepared = self.prepare_root_repair(offer_id, selected_root)?;
        self.commit_root_repair(prepared).map(|_| ())
    }

    pub(crate) fn pending_root_reset(&self) -> Result<Option<Uuid>, SharingError> {
        self.store
            .payload()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        Ok(self
            .snapshot
            .pending_root_reset
            .as_ref()
            .map(|pending| pending.folder_id))
    }

    /// Clear the reset intent only after the authenticated reset response and
    /// exact owned worker reap. Root identity is checked again at this durable
    /// boundary.
    pub(crate) fn complete_root_reset(&mut self, folder_id: Uuid) -> Result<(), SharingError> {
        let pending = self
            .snapshot
            .pending_root_reset
            .as_ref()
            .ok_or(SharingError::InvalidState)?;
        if pending.folder_id != folder_id {
            return Err(SharingError::InvalidState);
        }
        let observed = capture_root(&pending.replacement.path, &self.installation)?;
        if !same_root(&observed, &pending.replacement) {
            return Err(SharingError::FolderUnavailable);
        }
        let mut next = self.snapshot.clone();
        next.pending_root_reset = None;
        self.persist(next)
    }

    /// Durable revocation barrier, called before revoking the Covalent peer and
    /// after closing the worker lifeline. Restart cannot restore these offers.
    pub fn remove_peer(&mut self, peer_id: DeviceId) -> Result<(), SharingError> {
        let mut next = self.snapshot.clone();
        let mut changed = false;
        for share in &mut next.shares {
            if share.peer_identity.device_id == peer_id && !share.removed {
                share.removed = true;
                changed = true;
            }
        }
        let pending_before = next.pending_remote_removals.len();
        next.pending_remote_removals
            .retain(|pending| pending.peer_identity.device_id != peer_id);
        changed |= next.pending_remote_removals.len() != pending_before;
        if changed {
            self.persist(next)?;
        }
        Ok(())
    }

    /// Build complete desired settings after a fresh trust reconciliation and
    /// local root-identity check. Pending, paused and removed shares contribute
    /// no engine membership. No raw stored settings may bypass this method.
    pub fn desired_settings(&mut self) -> Result<EngineSessionSettings, SharingError> {
        self.reconcile_trust()?;
        self.store
            .payload()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        let mut peers = BTreeMap::<DeviceId, EnginePeerConfig>::new();
        let mut folders = BTreeMap::<Uuid, (String, PathBuf, BTreeSet<EngineDeviceId>)>::new();
        let own_id = self.installation.device_id().clone();
        let configuration = self
            .engine
            .config()
            .map_err(|_| SharingError::InvalidState)?;
        for share in self
            .snapshot
            .shares
            .iter()
            .filter(|s| !s.removed && !s.paused && s.commit.is_some())
        {
            let current_trust = observe_trust(&configuration, share.peer_identity.device_id)?
                .ok_or(SharingError::UntrustedPeer)?;
            if !current_trust.matches(share) {
                return Err(SharingError::UntrustedPeer);
            }
            let root = share.root.as_ref().ok_or(SharingError::InvalidState)?;
            validate_current_root(root)?;
            let peer_binding = if share.offer.source_device_id == self.engine.device_id() {
                &share
                    .acceptance
                    .as_ref()
                    .ok_or(SharingError::InvalidState)?
                    .target_engine
            } else {
                &share.offer.source_engine
            };
            let id = EngineDeviceId::parse(peer_binding.engine_device_id.as_str())
                .map_err(|_| SharingError::InvalidRecord)?;
            let address = peer_binding
                .validate()
                .map_err(|_| SharingError::InvalidRecord)?;
            let name = &configuration
                .trusted_peers
                .get(&share.peer_identity.device_id)
                .ok_or(SharingError::UntrustedPeer)?
                .display_name;
            let peer = EnginePeerConfig::new(id.clone(), name, address)
                .map_err(|_| SharingError::InvalidRecord)?;
            if let Some(old) = peers.insert(share.peer_identity.device_id, peer)
                && (old.id() != &id || old.address() != address)
            {
                return Err(SharingError::InvalidRecord);
            }
            let folder = folders.entry(share.offer.folder_id).or_insert_with(|| {
                (
                    share.offer.label.clone(),
                    root.path.clone(),
                    BTreeSet::from([own_id.clone()]),
                )
            });
            if folder.0 != share.offer.label || folder.1 != root.path {
                return Err(SharingError::InvalidState);
            }
            folder.2.insert(id);
        }
        let folders = folders
            .into_iter()
            .map(|(id, (label, root, members))| {
                EngineFolderConfig::new(id, &label, root, members.into_iter().collect())
                    .map_err(|_| SharingError::FolderUnavailable)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let listener = (!folders.is_empty()).then_some(self.snapshot.listener);
        Ok(EngineSessionSettings {
            device_name: configuration.device_name,
            listener,
            peers: peers.into_values().collect(),
            folders,
        })
    }

    fn trusted_peer(&self, id: DeviceId) -> Result<CurrentPeerTrust, SharingError> {
        // Existing authenticated transport pin is mandatory. Backup-only legacy
        // trust without that pin must complete normal verified pairing first.
        let config = self
            .engine
            .config()
            .map_err(|_| SharingError::InvalidState)?;
        observe_trust(&config, id)?.ok_or(SharingError::UntrustedPeer)
    }

    fn reconcile_trust(&mut self) -> Result<(), SharingError> {
        self.store
            .payload()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        let mut next = self.snapshot.clone();
        let mut changed = false;
        let config = self
            .engine
            .config()
            .map_err(|_| SharingError::InvalidState)?;
        for share in &mut next.shares {
            if !share.removed
                && !observe_trust(&config, share.peer_identity.device_id)?
                    .is_some_and(|current| current.matches(share))
            {
                share.removed = true;
                changed = true;
            }
        }
        let pending_before = next.pending_remote_removals.len();
        let mut retained = Vec::with_capacity(pending_before);
        for pending in std::mem::take(&mut next.pending_remote_removals) {
            if observe_trust(&config, pending.peer_identity.device_id)?.is_some_and(|current| {
                current.identity == pending.peer_identity
                    && current.transport == pending.peer_transport
                    && current.confirmed_at_unix_ms == pending.peer_confirmed_at_unix_ms
            }) {
                retained.push(pending);
            }
        }
        next.pending_remote_removals = retained;
        changed |= next.pending_remote_removals.len() != pending_before;
        if changed {
            self.persist(next)?;
        }
        Ok(())
    }

    fn check_capacity(&self) -> Result<(), SharingError> {
        if self.snapshot.shares.len() >= MAX_SHARES
            || retained_offer_count(&self.snapshot)? >= MAX_RETAINED_OFFERS
        {
            Err(SharingError::LimitExceeded)
        } else {
            Ok(())
        }
    }

    fn check_renewal_capacity(&self, share: &Share) -> Result<(), SharingError> {
        if share.superseded_offers.len() >= MAX_RENEWALS_PER_SHARE
            || retained_offer_count(&self.snapshot)? >= MAX_RETAINED_OFFERS
        {
            Err(SharingError::LimitExceeded)
        } else {
            Ok(())
        }
    }

    fn check_not_shared(&self, folder_id: Uuid, peer: DeviceId) -> Result<(), SharingError> {
        if self.snapshot.shares.iter().any(|s| {
            !s.removed && s.offer.folder_id == folder_id && s.peer_identity.device_id == peer
        }) {
            Err(SharingError::AlreadyShared)
        } else {
            Ok(())
        }
    }

    fn index(&self, offer: Uuid) -> Result<usize, SharingError> {
        if self.snapshot.tombstones.iter().any(|removed| {
            removed.offer_id == offer || removed.superseded_offer_ids.contains(&offer)
        }) {
            return Err(SharingError::Removed);
        }
        if self.snapshot.shares.iter().any(|share| {
            share
                .superseded_offers
                .iter()
                .any(|superseded| superseded.offer_id == offer)
        }) {
            return Err(SharingError::Removed);
        }
        let index = self
            .snapshot
            .shares
            .iter()
            .position(|s| s.offer.offer_id == offer)
            .ok_or(SharingError::InvalidRecord)?;
        if self.snapshot.shares[index].removed {
            return Err(SharingError::Removed);
        }
        Ok(index)
    }

    fn persist(&mut self, mut candidate: Snapshot) -> Result<(), SharingError> {
        if candidate
            .pending_root_reset
            .as_ref()
            .is_some_and(|pending| {
                candidate
                    .shares
                    .iter()
                    .find(|share| share.offer.offer_id == pending.offer_id)
                    .is_none_or(|share| share.removed)
            })
        {
            candidate.pending_root_reset = None;
        }
        compact_removed(&mut candidate);
        validate_snapshot(&candidate, &self.engine, &self.installation)?;
        let payload = serde_json::to_vec(&candidate).map_err(|_| SharingError::InvalidState)?;
        self.store
            .replace(&payload)
            .map_err(|_| SharingError::PersistenceUncertain)?;
        self.snapshot = candidate;
        Ok(())
    }

    fn advance_freshness_floor(&mut self, now: u64) -> Result<(), SharingError> {
        // A nondecreasing cutoff blocks old fresh intake even after wall-clock
        // rollback. Existing accepted records may still replay by exact match.
        let floor = freshness_floor(now);
        if floor > self.snapshot.freshness_floor_unix_ms {
            let mut next = self.snapshot.clone();
            advance_freshness_floor_candidate(&mut next, now);
            self.persist(next)?;
        }
        Ok(())
    }
}

fn validate_snapshot(
    snapshot: &Snapshot,
    engine: &Engine,
    installation: &EngineInstallation,
) -> Result<(), SharingError> {
    let advertised = snapshot
        .binding
        .validate()
        .map_err(|_| SharingError::InvalidState)?;
    if snapshot.schema_version != 1
        || snapshot.owner != engine.public_identity()
        || snapshot.binding.covalent_device_id != engine.device_id()
        || snapshot.binding.engine_device_id.as_str() != installation.device_id().as_str()
        || snapshot.listener.port() == 0
        || snapshot.listener.port() != advertised.port()
        || snapshot.listener.is_ipv4() != advertised.is_ipv4()
        || (!snapshot.listener.ip().is_unspecified() && snapshot.listener.ip() != advertised.ip())
        || snapshot.shares.len() > MAX_SHARES
        || retained_offer_count(snapshot)? > MAX_RETAINED_OFFERS
    {
        return Err(SharingError::InvalidState);
    }
    let mut offers = BTreeSet::new();
    let mut memberships = BTreeSet::new();
    let mut roots = BTreeMap::<Uuid, &LocalRoot>::new();
    let mut labels = BTreeMap::<Uuid, &str>::new();
    let mut peer_engines = BTreeMap::new();
    let mut engine_owners = BTreeMap::new();
    let mut removed_chain_total = 0_usize;
    for removed in &snapshot.tombstones {
        removed_chain_total = removed_chain_total
            .checked_add(removed.superseded_offer_ids.len())
            .ok_or(SharingError::LimitExceeded)?;
        if removed.offer_id.is_nil()
            || removed.superseded_offer_ids.len() > MAX_RENEWALS_PER_SHARE
            || removed.superseded_offer_ids.iter().any(Uuid::is_nil)
            || removed
                .superseded_offer_ids
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != removed.superseded_offer_ids.len()
            || removed.superseded_offer_ids.contains(&removed.offer_id)
            || removed.folder_id.is_nil()
            || removed.peer_id == engine.device_id()
            || removed.issued_at_unix_ms == 0
            || removed.label.is_empty()
            || removed.label.len() > 256
            || removed.label.chars().any(char::is_control)
            || !offers.insert(removed.offer_id)
        {
            return Err(SharingError::InvalidState);
        }
    }
    if removed_chain_total > MAX_RETAINED_OFFERS {
        return Err(SharingError::LimitExceeded);
    }
    for share in &snapshot.shares {
        let source_is_owner = share.offer.source_device_id == engine.device_id();
        let target_is_owner = share.offer.target_device_id == engine.device_id();
        if source_is_owner == target_is_owner
            || !offers.insert(share.offer.offer_id)
            || share.peer_identity.device_id == engine.device_id()
            || share.peer_transport.peer_id != share.peer_identity.device_id
            || share.peer_confirmed_at_unix_ms == 0
        {
            return Err(SharingError::InvalidState);
        }
        let (source, target) = if source_is_owner {
            (&snapshot.owner, &share.peer_identity)
        } else {
            (&share.peer_identity, &snapshot.owner)
        };
        verify_folder_share_offer(&share.offer, source, target.device_id)
            .map_err(|_| SharingError::InvalidRecord)?;
        if share.superseded_offers.len() > MAX_RENEWALS_PER_SHARE {
            return Err(SharingError::LimitExceeded);
        }
        let mut previous_issued_at = 0;
        for superseded in &share.superseded_offers {
            if superseded.offer_id.is_nil()
                || superseded.offer_id == share.offer.offer_id
                || superseded.issued_at_unix_ms == 0
                || superseded.issued_at_unix_ms <= previous_issued_at
                || superseded.issued_at_unix_ms >= share.offer.issued_at_unix_ms
                || !offers.insert(superseded.offer_id)
            {
                return Err(SharingError::InvalidState);
            }
            previous_issued_at = superseded.issued_at_unix_ms;
        }
        if source_is_owner && share.offer.source_engine != snapshot.binding {
            return Err(SharingError::InvalidRecord);
        }
        if let Some(acceptance) = &share.acceptance {
            verify_folder_share_acceptance(&share.offer, acceptance, source, target)
                .map_err(|_| SharingError::InvalidRecord)?;
            if target_is_owner && acceptance.target_engine != snapshot.binding {
                return Err(SharingError::InvalidRecord);
            }
        }
        if let Some(commit) = &share.commit {
            verify_folder_share_commit(
                &share.offer,
                share
                    .acceptance
                    .as_ref()
                    .ok_or(SharingError::InvalidState)?,
                commit,
                source,
                target,
            )
            .map_err(|_| SharingError::InvalidRecord)?;
        }
        if (source_is_owner || share.acceptance.is_some()) && share.root.is_none()
            || (!source_is_owner && share.acceptance.is_none() && share.root.is_some())
        {
            return Err(SharingError::InvalidState);
        }
        if !share.removed {
            if let Some(acceptance) = &share.acceptance {
                let binding = if source_is_owner {
                    &acceptance.target_engine
                } else {
                    &share.offer.source_engine
                };
                if binding.engine_device_id.as_str() == installation.device_id().as_str() {
                    return Err(SharingError::InvalidRecord);
                }
                if let Some(old) = peer_engines.insert(share.peer_identity.device_id, binding)
                    && old != binding
                {
                    return Err(SharingError::InvalidRecord);
                }
                if let Some(old) =
                    engine_owners.insert(&binding.engine_device_id, share.peer_identity.device_id)
                    && old != share.peer_identity.device_id
                {
                    return Err(SharingError::InvalidRecord);
                }
            }
            if let Some(old) = labels.insert(share.offer.folder_id, &share.offer.label)
                && old != share.offer.label
            {
                return Err(SharingError::InvalidRecord);
            }
            if !memberships.insert((share.offer.folder_id, share.peer_identity.device_id)) {
                return Err(SharingError::InvalidState);
            }
            if let Some(root) = &share.root {
                if !root.path.is_absolute()
                    || root.path.as_os_str().len() > 4096
                    || root
                        .path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    || root.path.starts_with(installation.root())
                    || installation.root().starts_with(&root.path)
                {
                    return Err(SharingError::InvalidState);
                }
                if let Some(old) = roots.insert(share.offer.folder_id, root)
                    && !same_root(old, root)
                {
                    return Err(SharingError::InvalidState);
                }
            }
        }
    }
    let mut removed_chain_ids = BTreeSet::new();
    for removed in &snapshot.tombstones {
        for offer_id in &removed.superseded_offer_ids {
            if !removed_chain_ids.insert(*offer_id)
                || snapshot.shares.iter().any(|share| {
                    share.offer.offer_id == *offer_id
                        || share
                            .superseded_offers
                            .iter()
                            .any(|superseded| superseded.offer_id == *offer_id)
                })
                || snapshot.tombstones.iter().any(|candidate| {
                    candidate.offer_id == *offer_id
                        && (candidate.peer_id != removed.peer_id
                            || candidate.folder_id != removed.folder_id
                            || candidate.incoming != removed.incoming)
                })
            {
                return Err(SharingError::InvalidState);
            }
        }
    }
    if snapshot.pending_remote_removals.len() > MAX_PENDING_REMOVALS {
        return Err(SharingError::LimitExceeded);
    }
    let mut pending_offer_ids = BTreeSet::new();
    let mut pending_total = 0_usize;
    for pending in &snapshot.pending_remote_removals {
        pending_total = pending_total
            .checked_add(pending.offer_ids.len())
            .ok_or(SharingError::LimitExceeded)?;
        if pending.peer_identity.device_id == engine.device_id()
            || pending.peer_transport.peer_id != pending.peer_identity.device_id
            || pending.peer_confirmed_at_unix_ms == 0
            || pending.offer_source_id != engine.device_id()
                && pending.offer_source_id != pending.peer_identity.device_id
            || pending.folder_id.is_nil()
            || pending.offer_ids.is_empty()
            || pending.offer_ids.len() > MAX_REMOVAL_OFFER_IDS
            || pending.offer_ids.iter().any(Uuid::is_nil)
            || snapshot
                .tombstones
                .iter()
                .find(|removed| {
                    Some(&removed.offer_id) == pending.offer_ids.last()
                        && removed.folder_id == pending.folder_id
                        && removed.peer_id == pending.peer_identity.device_id
                        && (if removed.incoming {
                            removed.peer_id
                        } else {
                            engine.device_id()
                        }) == pending.offer_source_id
                })
                .is_none_or(|current| {
                    current.superseded_offer_ids.as_slice()
                        != &pending.offer_ids[..pending.offer_ids.len() - 1]
                })
            || pending.offer_ids.iter().any(|offer_id| {
                !pending_offer_ids.insert(*offer_id)
                    || !snapshot.tombstones.iter().any(|removed| {
                        removed.offer_id == *offer_id
                            && removed.folder_id == pending.folder_id
                            && removed.peer_id == pending.peer_identity.device_id
                            && (if removed.incoming {
                                removed.peer_id
                            } else {
                                engine.device_id()
                            }) == pending.offer_source_id
                    })
            })
        {
            return Err(SharingError::InvalidState);
        }
    }
    if pending_total > MAX_RETAINED_OFFERS {
        return Err(SharingError::LimitExceeded);
    }
    if snapshot.remote_removal_tombstones.len() > MAX_RETAINED_OFFERS {
        return Err(SharingError::LimitExceeded);
    }
    let mut remote_ids = BTreeSet::new();
    let mut remote_per_peer = BTreeMap::<DeviceId, usize>::new();
    let mut remote_total = 0_usize;
    for removed in &snapshot.remote_removal_tombstones {
        remote_total = remote_total
            .checked_add(removed.offer_ids.len())
            .ok_or(SharingError::LimitExceeded)?;
        let count = remote_per_peer.entry(removed.peer_id).or_default();
        *count = count.checked_add(1).ok_or(SharingError::LimitExceeded)?;
        if removed.peer_id == engine.device_id()
            || removed.offer_source_id != engine.device_id()
                && removed.offer_source_id != removed.peer_id
            || removed.folder_id.is_nil()
            || removed.offer_ids.is_empty()
            || removed.offer_ids.len() > MAX_REMOVAL_OFFER_IDS
            || *count > MAX_SHARES
            || removed.offer_ids.iter().any(Uuid::is_nil)
            || removed.offer_ids.iter().any(|offer_id| {
                !remote_ids.insert(*offer_id)
                    || retained_offer_binding(snapshot, *offer_id).is_some_and(|binding| {
                        binding != (removed.peer_id, removed.folder_id, removed.offer_source_id)
                    })
            })
        {
            return Err(SharingError::InvalidState);
        }
    }
    if remote_total > MAX_RETAINED_OFFERS {
        return Err(SharingError::LimitExceeded);
    }
    for (id, root) in &roots {
        if roots.iter().any(|(other, path)| {
            id != other && (root.path.starts_with(&path.path) || path.path.starts_with(&root.path))
        }) {
            return Err(SharingError::FolderUnavailable);
        }
    }
    if roots.len() > 128 || peer_engines.len() > 128 {
        return Err(SharingError::LimitExceeded);
    }
    if let Some(pending) = &snapshot.pending_root_reset {
        let Some((index, share)) = snapshot
            .shares
            .iter()
            .enumerate()
            .find(|(_, share)| share.offer.offer_id == pending.offer_id)
        else {
            return Err(SharingError::InvalidState);
        };
        if pending.folder_id != share.offer.folder_id
            || share.removed
            || share.paused
            || share.commit.is_none()
            || share
                .root
                .as_ref()
                .is_none_or(|root| !same_root(root, &pending.replacement))
            || snapshot
                .shares
                .iter()
                .enumerate()
                .any(|(other, candidate)| {
                    other != index
                        && !candidate.removed
                        && candidate.offer.folder_id == pending.folder_id
                })
        {
            return Err(SharingError::InvalidState);
        }
    }
    Ok(())
}

fn validate_removal_notice(
    notice: &FolderRemovalNotice,
    expected_target: DeviceId,
) -> Result<(), SharingError> {
    if notice.requester_id == expected_target
        || notice.target_id != expected_target
        || notice.offer_source_id != notice.requester_id
            && notice.offer_source_id != notice.target_id
        || notice.folder_id.is_nil()
        || notice.offer_ids.is_empty()
        || notice.offer_ids.len() > MAX_REMOVAL_OFFER_IDS
        || notice.offer_ids.iter().any(Uuid::is_nil)
        || notice.offer_ids.iter().collect::<BTreeSet<_>>().len() != notice.offer_ids.len()
    {
        return Err(SharingError::InvalidRecord);
    }
    Ok(())
}

fn capture_root(path: &Path, installation: &EngineInstallation) -> Result<LocalRoot, SharingError> {
    let admitted = EngineFolderConfig::new(
        Uuid::new_v4(),
        "Selected folder",
        path.to_path_buf(),
        vec![installation.device_id().clone()],
    )
    .map_err(|_| SharingError::FolderUnavailable)?;
    let path = admitted.root();
    if path.starts_with(installation.root()) || installation.root().starts_with(path) {
        return Err(SharingError::FolderUnavailable);
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| SharingError::FolderUnavailable)?;
    Ok(LocalRoot {
        path: path.to_path_buf(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn same_root(left: &LocalRoot, right: &LocalRoot) -> bool {
    left.path == right.path && left.device == right.device && left.inode == right.inode
}

fn validate_current_root(root: &LocalRoot) -> Result<(), SharingError> {
    let canonical =
        std::fs::canonicalize(&root.path).map_err(|_| SharingError::FolderUnavailable)?;
    let metadata =
        std::fs::symlink_metadata(&root.path).map_err(|_| SharingError::FolderUnavailable)?;
    if canonical != root.path
        || !metadata.is_dir()
        || (metadata.dev(), metadata.ino()) != (root.device, root.inode)
    {
        return Err(SharingError::FolderUnavailable);
    }
    Ok(())
}

struct CurrentPeerTrust {
    identity: PublicIdentity,
    transport: TransportBinding,
    confirmed_at_unix_ms: u64,
}

impl CurrentPeerTrust {
    fn matches(&self, share: &Share) -> bool {
        self.identity == share.peer_identity
            && self.transport == share.peer_transport
            && self.confirmed_at_unix_ms == share.peer_confirmed_at_unix_ms
    }
}

// All trust fields come from one atomic Engine configuration snapshot. Internal
// read/identity errors are unavailable state, never a durable revocation verdict.
fn observe_trust(
    config: &NodeConfig,
    id: DeviceId,
) -> Result<Option<CurrentPeerTrust>, SharingError> {
    let Some(grant) = config.trusted_peers.get(&id) else {
        return Ok(None);
    };
    let Some(transport) = config.trusted_peer_transports.get(&id) else {
        return Ok(None);
    };
    if grant.revoked {
        return Ok(None);
    }
    if grant.peer_device_id != id || transport.peer_id != id || grant.confirmed_at_unix_ms == 0 {
        return Err(SharingError::InvalidState);
    }
    let identity = PublicIdentity::from_encoded(id, grant.public_key.clone())
        .map_err(|_| SharingError::InvalidState)?;
    Ok(Some(CurrentPeerTrust {
        identity,
        transport: transport.clone(),
        confirmed_at_unix_ms: grant.confirmed_at_unix_ms,
    }))
}

fn compact_removed(snapshot: &mut Snapshot) {
    let owner = snapshot.owner.device_id;
    let pending_ids = snapshot
        .pending_remote_removals
        .iter()
        .flat_map(|pending| pending.offer_ids.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut retained = Vec::with_capacity(snapshot.shares.len());
    for share in snapshot.shares.drain(..) {
        if share.removed {
            let superseded_offer_ids = share
                .superseded_offers
                .iter()
                .map(|superseded| superseded.offer_id)
                .collect::<Vec<_>>();
            for superseded in share.superseded_offers {
                if superseded.issued_at_unix_ms >= snapshot.freshness_floor_unix_ms
                    || pending_ids.contains(&superseded.offer_id)
                {
                    snapshot.tombstones.push(Tombstone {
                        offer_id: superseded.offer_id,
                        superseded_offer_ids: Vec::new(),
                        folder_id: share.offer.folder_id,
                        label: share.offer.label.clone(),
                        peer_id: share.peer_identity.device_id,
                        incoming: share.offer.target_device_id == owner,
                        issued_at_unix_ms: superseded.issued_at_unix_ms,
                    });
                }
            }
            // Retain the canonical terminal row under the existing finite offer
            // quota so native capability cleanup never has to infer removal
            // from an absent status row. Individual renewal tombstones may age.
            snapshot.tombstones.push(Tombstone {
                offer_id: share.offer.offer_id,
                superseded_offer_ids,
                folder_id: share.offer.folder_id,
                label: share.offer.label,
                peer_id: share.peer_identity.device_id,
                incoming: share.offer.target_device_id == owner,
                issued_at_unix_ms: share.offer.issued_at_unix_ms,
            });
        } else {
            retained.push(share);
        }
    }
    snapshot.shares = retained;
}

fn freshness_floor(now: u64) -> u64 {
    now.saturating_sub(INVITATION_LIFETIME_MS + 5 * 60 * 1000)
}

fn advance_freshness_floor_candidate(snapshot: &mut Snapshot, now: u64) {
    let floor = freshness_floor(now);
    if floor > snapshot.freshness_floor_unix_ms {
        snapshot.freshness_floor_unix_ms = floor;
        let pending_ids = snapshot
            .pending_remote_removals
            .iter()
            .flat_map(|pending| pending.offer_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        let superseded_ids = snapshot
            .tombstones
            .iter()
            .flat_map(|removed| removed.superseded_offer_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        snapshot.tombstones.retain(|removed| {
            !superseded_ids.contains(&removed.offer_id)
                || removed.issued_at_unix_ms >= floor
                || pending_ids.contains(&removed.offer_id)
        });
    }
}

fn is_prefix<T: PartialEq>(prefix: &[T], sequence: &[T]) -> bool {
    prefix.len() <= sequence.len() && sequence[..prefix.len()] == *prefix
}

fn retained_offer_count(snapshot: &Snapshot) -> Result<usize, SharingError> {
    snapshot.shares.iter().try_fold(
        snapshot
            .shares
            .len()
            .checked_add(snapshot.tombstones.len())
            .ok_or(SharingError::LimitExceeded)?,
        |total, share| {
            total
                .checked_add(share.superseded_offers.len())
                .ok_or(SharingError::LimitExceeded)
        },
    )
}

fn retained_offer_binding(
    snapshot: &Snapshot,
    offer_id: Uuid,
) -> Option<(DeviceId, Uuid, DeviceId)> {
    if let Some(removed) = snapshot.tombstones.iter().find(|removed| {
        removed.offer_id == offer_id || removed.superseded_offer_ids.contains(&offer_id)
    }) {
        return Some((
            removed.peer_id,
            removed.folder_id,
            if removed.incoming {
                removed.peer_id
            } else {
                snapshot.owner.device_id
            },
        ));
    }
    snapshot.shares.iter().find_map(|share| {
        (share.offer.offer_id == offer_id
            || share
                .superseded_offers
                .iter()
                .any(|superseded| superseded.offer_id == offer_id))
        .then_some((
            share.peer_identity.device_id,
            share.offer.folder_id,
            share.offer.source_device_id,
        ))
    })
}

#[cfg(test)]
#[path = "sharing_tests.rs"]
mod tests;
