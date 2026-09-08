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
    pub folder_id: Uuid,
    pub label: String,
    pub peer_id: DeviceId,
    pub incoming: bool,
    pub phase: SharingPhase,
    /// Only unaccepted invitations expire. A durable acceptance remains valid
    /// for replay after its original delivery window has elapsed.
    pub expires_at_unix_ms: Option<u64>,
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
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Tombstone {
    offer_id: Uuid,
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
            })
            .collect();
        summaries.extend(self.snapshot.tombstones.iter().map(|removed| ShareSummary {
            offer_id: removed.offer_id,
            folder_id: removed.folder_id,
            label: removed.label.clone(),
            peer_id: removed.peer_id,
            incoming: removed.incoming,
            phase: SharingPhase::Removed,
            expires_at_unix_ms: None,
        }));
        Ok(summaries)
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
            .tombstones
            .iter()
            .any(|removed| removed.offer_id == offer.offer_id)
        {
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
        verify_fresh_folder_share_offer(&offer, &peer.identity, self.engine.device_id(), now)
            .map_err(|_| SharingError::InvalidRecord)?;
        self.advance_freshness_floor(now)?;
        if offer.issued_at_unix_ms < self.snapshot.freshness_floor_unix_ms {
            return Err(SharingError::Removed);
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
        });
        self.persist(next)
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
        next.shares[index].removed = true;
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
        if changed {
            self.persist(next)?;
        }
        Ok(())
    }

    fn check_capacity(&self) -> Result<(), SharingError> {
        if self.snapshot.shares.len() >= MAX_SHARES
            || self.snapshot.shares.len() + self.snapshot.tombstones.len() >= MAX_RETAINED_OFFERS
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
        if self
            .snapshot
            .tombstones
            .iter()
            .any(|removed| removed.offer_id == offer)
        {
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
        let floor = now.saturating_sub(INVITATION_LIFETIME_MS + 5 * 60 * 1000);
        if floor > self.snapshot.freshness_floor_unix_ms {
            let mut next = self.snapshot.clone();
            next.freshness_floor_unix_ms = floor;
            next.tombstones
                .retain(|removed| removed.issued_at_unix_ms >= floor);
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
        || snapshot.shares.len() + snapshot.tombstones.len() > MAX_RETAINED_OFFERS
    {
        return Err(SharingError::InvalidState);
    }
    let mut offers = BTreeSet::new();
    let mut memberships = BTreeSet::new();
    let mut roots = BTreeMap::<Uuid, &LocalRoot>::new();
    let mut labels = BTreeMap::<Uuid, &str>::new();
    let mut peer_engines = BTreeMap::new();
    let mut engine_owners = BTreeMap::new();
    for removed in &snapshot.tombstones {
        if removed.offer_id.is_nil()
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
    let mut retained = Vec::with_capacity(snapshot.shares.len());
    for share in snapshot.shares.drain(..) {
        if share.removed {
            if share.offer.issued_at_unix_ms >= snapshot.freshness_floor_unix_ms {
                snapshot.tombstones.push(Tombstone {
                    offer_id: share.offer.offer_id,
                    folder_id: share.offer.folder_id,
                    label: share.offer.label,
                    peer_id: share.peer_identity.device_id,
                    incoming: share.offer.target_device_id == owner,
                    issued_at_unix_ms: share.offer.issued_at_unix_ms,
                });
            }
        } else {
            retained.push(share);
        }
    }
    snapshot.shares = retained;
}

#[cfg(test)]
#[path = "sharing_tests.rs"]
mod tests;
