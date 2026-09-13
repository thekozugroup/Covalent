use super::*;
use crate::sync_engine::{EngineIndexSnapshot, FolderSyncAccessRecovery};
use crate::transport::TlsIdentity;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{EngineOptions, StaticKeyProtector};
use covalent_protocol::{PeerRole, TransportBinding};
use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant};

struct Device {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    engine: Arc<Engine>,
    installation: Arc<EngineInstallation>,
    protector: Arc<dyn KeyProtector>,
    address: SocketAddr,
}

impl Device {
    fn new(name: &str, port: u16) -> Self {
        let temporary = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let protector: Arc<dyn KeyProtector> =
            Arc::new(StaticKeyProtector::new(1, [21; 32]).unwrap());
        let engine = Arc::new(
            Engine::open(EngineOptions {
                initial_device_name: name.into(),
                ..EngineOptions::new(root.join("node")).with_key_protector(Arc::clone(&protector))
            })
            .unwrap(),
        );
        let sync = root.join("sync");
        std::fs::create_dir(&sync).unwrap();
        std::fs::set_permissions(&sync, std::fs::Permissions::from_mode(0o700)).unwrap();
        let installation = Arc::new(EngineInstallation::create(&sync, protector.as_ref()).unwrap());
        std::fs::create_dir(root.join("files")).unwrap();
        Self {
            _temporary: temporary,
            root,
            engine,
            installation,
            protector,
            address: ([127, 0, 0, 1], port).into(),
        }
    }
    fn journal(&self) -> FolderSharingJournal {
        FolderSharingJournal::create(
            Arc::clone(&self.engine),
            Arc::clone(&self.installation),
            Arc::clone(&self.protector),
            self.address,
            self.address,
        )
        .unwrap()
    }
    fn reopen(&self) -> FolderSharingJournal {
        FolderSharingJournal::open(
            Arc::clone(&self.engine),
            Arc::clone(&self.installation),
            Arc::clone(&self.protector),
        )
        .unwrap()
    }
    fn files(&self) -> PathBuf {
        self.root.join("files")
    }
    fn transport(&self) -> TransportBinding {
        let tls =
            TlsIdentity::load_or_create(self.root.join("tls"), &self.root, self.protector.as_ref())
                .unwrap();
        TransportBinding {
            peer_id: self.engine.device_id(),
            display_name: self.engine.config().unwrap().device_name,
            address: self.address.to_string(),
            certificate_der: URL_SAFE_NO_PAD.encode(tls.certificate_der()),
            certificate_fingerprint: tls.certificate_fingerprint(),
        }
    }
}

fn pair(first: &Device, second: &Device) {
    pair_at(first, second, 1000);
}

fn pair_at(first: &Device, second: &Device, now: u64) {
    let invitation = first
        .engine
        .pairing_manager()
        .create_invitation_with_transport(
            now,
            60_000,
            vec![first.address.to_string()],
            first.transport(),
        )
        .unwrap();
    let roles = BTreeSet::from([PeerRole::BackupReader]);
    let mut session = second
        .engine
        .accept_pairing_with_transport(
            invitation,
            second.transport(),
            roles.clone(),
            roles,
            now + 1,
        )
        .unwrap();
    let code = session.authentication_string().as_str().to_owned();
    second
        .engine
        .confirm_pairing_as_responder(&mut session, &code, now + 2)
        .unwrap();
    first
        .engine
        .confirm_pairing_as_inviter(&mut session, &code, now + 3)
        .unwrap();
    first
        .engine
        .finalize_pairing_as_inviter(&session, now + 4)
        .unwrap();
    second
        .engine
        .finalize_pairing_as_responder(&session, now + 4)
        .unwrap();
}

fn share(
    first: &Device,
    second: &Device,
    a: &mut FolderSharingJournal,
    b: &mut FolderSharingJournal,
) -> (FolderShareOffer, FolderShareAcceptance, FolderShareCommit) {
    let offer = a
        .offer(
            second.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &first.files(),
            2000,
        )
        .unwrap();
    b.receive_offer(offer.clone(), 2001).unwrap();
    let accepted = b.accept(offer.offer_id, &second.files(), 2002).unwrap();
    let committed = a
        .receive_acceptance(offer.offer_id, accepted.clone(), 2003)
        .unwrap();
    b.receive_commit(offer.offer_id, committed.clone()).unwrap();
    (offer, accepted, committed)
}

#[test]
fn one_way_policy_drives_fanout_roles_and_survives_reopen() {
    let a = Device::new("Source", 43211);
    let b = Device::new("Destination", 43212);
    let c = Device::new("Second destination", 43213);
    pair(&a, &b);
    pair(&a, &c);
    pair(&b, &c);
    let mut first = a.journal();
    let mut second = b.journal();
    let mut third = c.journal();
    let folder_id = Uuid::new_v4();
    let policy = covalent_protocol::FolderLinkPolicy::default();
    for (index, (device, journal)) in [(&b, &mut second), (&c, &mut third)]
        .into_iter()
        .enumerate()
    {
        let now = 2000 + index as u64 * 10;
        let offer = first
            .offer_with_policy(
                device.engine.device_id(),
                folder_id,
                "Photos",
                &a.files(),
                now,
                Some(policy),
            )
            .unwrap();
        journal.receive_offer(offer.clone(), now + 1).unwrap();
        let acceptance = journal
            .accept(offer.offer_id, &device.files(), now + 2)
            .unwrap();
        let commit = first
            .receive_acceptance(offer.offer_id, acceptance, now + 3)
            .unwrap();
        journal.receive_commit(offer.offer_id, commit).unwrap();
        assert!(journal.desired_settings().unwrap().folders.is_empty());
        deliver_link_settings(&mut first, journal);
    }
    let source = first.desired_settings().unwrap();
    assert_eq!(source.folders.len(), 1);
    assert_eq!(source.folders[0].members().len(), 3);
    assert_eq!(source.folders[0].role(), EngineFolderRole::Source);
    assert!(
        first
            .offer_with_policy(
                b.engine.device_id(),
                folder_id,
                "Photos",
                &a.files(),
                2024,
                Some(covalent_protocol::FolderLinkPolicy {
                    propagate_source_deletions: true,
                    ..policy
                })
            )
            .is_err()
    );
    assert!(
        second
            .offer_with_policy(
                c.engine.device_id(),
                folder_id,
                "Photos",
                &b.files(),
                2024,
                Some(policy)
            )
            .is_err()
    );
    drop(second);
    let mut reopened = b.reopen();
    assert_eq!(reopened.summaries().unwrap()[0].link_policy, Some(policy));
    let target = reopened.desired_settings().unwrap();
    assert_eq!(target.folders[0].members().len(), 2);
    assert_eq!(
        target.folders[0].role(),
        EngineFolderRole::Destination {
            propagate_source_deletions: false,
            restore_local_deletions: false,
        }
    );
}

fn deliver_link_settings(source: &mut FolderSharingJournal, target: &mut FolderSharingJournal) {
    for delivery in source.outbound_records().unwrap() {
        if delivery.peer_transport.peer_id == target.engine.device_id()
            && let FolderShareRecord::SettingsCommit(commit) = delivery.record
        {
            target.receive_link_settings_commit(&commit).unwrap();
        }
    }
}

#[test]
fn link_settings_commit_once_fan_out_survive_offline_conflicts_and_require_membership() {
    let a = Device::new("Source", 43211);
    let b = Device::new("Phone", 43212);
    let c = Device::new("Tablet", 43213);
    let outsider = Device::new("Another paired device", 43214);
    pair(&a, &b);
    pair(&a, &c);
    pair(&a, &outsider);
    let mut source = a.journal();
    let mut phone = b.journal();
    let mut tablet = c.journal();
    let folder = Uuid::new_v4();
    let policy = covalent_protocol::FolderLinkPolicy::default();
    for (device, journal) in [(&b, &mut phone), (&c, &mut tablet)] {
        let offer = source
            .offer_with_policy(
                device.engine.device_id(),
                folder,
                "Photos",
                &a.files(),
                2000,
                Some(policy),
            )
            .unwrap();
        journal.receive_offer(offer.clone(), 2001).unwrap();
        let acceptance = journal
            .accept(offer.offer_id, &device.files(), 2002)
            .unwrap();
        let commit = source
            .receive_acceptance(offer.offer_id, acceptance, 2003)
            .unwrap();
        journal.receive_commit(offer.offer_id, commit).unwrap();
        assert!(journal.desired_settings().unwrap().folders.is_empty());
        deliver_link_settings(&mut source, journal);
        assert_eq!(journal.desired_settings().unwrap().folders.len(), 1);
    }
    let settings = FolderLinkSettings {
        deletion_policy: covalent_protocol::FolderLinkPolicy {
            propagate_source_deletions: true,
            restore_local_deletions: true,
        },
        paused: true,
        cadence: LinkCadence::Continuous,
        android_conditions: AndroidLinkConditions::default(),
    };
    let change = Uuid::new_v4();
    phone
        .request_link_settings(folder, change, 0, settings)
        .unwrap();
    let phone_offer = phone.summaries().unwrap()[0].offer_id;
    phone.set_paused(phone_offer, true).unwrap();
    assert_eq!(
        phone.set_paused(phone_offer, false),
        Err(SharingError::SettingsPending)
    );
    phone
        .request_link_settings(folder, change, 0, settings)
        .unwrap();
    assert_eq!(
        phone.summaries().unwrap()[0]
            .link_settings
            .as_ref()
            .unwrap()
            .revision,
        0
    );
    assert!(
        !phone.summaries().unwrap()[0]
            .link_settings
            .as_ref()
            .unwrap()
            .settings
            .paused
    );
    assert_eq!(
        phone.request_link_settings(folder, Uuid::new_v4(), 0, settings),
        Err(SharingError::SettingsPending)
    );
    drop(phone);
    let mut phone = b.reopen();
    let pending = phone.summaries().unwrap()[0]
        .link_settings
        .as_ref()
        .unwrap()
        .pending_change
        .clone()
        .unwrap();
    let mut unauthorized = pending.clone();
    unauthorized.requester_id = outsider.engine.device_id();
    assert_eq!(
        source.receive_link_settings_request(&unauthorized),
        Err(SharingError::UntrustedPeer)
    );
    assert_eq!(
        source.summaries().unwrap()[0]
            .link_settings
            .as_ref()
            .unwrap()
            .revision,
        0
    );
    let committed = source.receive_link_settings_request(&pending).unwrap();
    assert_eq!(committed.revision, 1);
    assert_eq!(
        source.receive_link_settings_request(&pending).unwrap(),
        committed
    );
    assert!(source.desired_settings().unwrap().folders.is_empty());
    phone.receive_link_settings_commit(&committed).unwrap();
    assert!(
        phone.summaries().unwrap()[0]
            .link_settings
            .as_ref()
            .unwrap()
            .pending_change
            .is_none()
    );
    deliver_link_settings(&mut source, &mut tablet);
    for journal in [&mut phone, &mut tablet] {
        assert!(journal.desired_settings().unwrap().folders.is_empty());
        let state = journal.summaries().unwrap()[0]
            .link_settings
            .clone()
            .unwrap();
        assert_eq!(state.revision, 1);
        assert_eq!(state.settings, settings);
    }
    // Two offline changes start at the same revision. The first commits, the
    // second never overwrites it and remains visible for user review.
    let resumed = FolderLinkSettings {
        paused: false,
        ..settings
    };
    phone
        .request_link_settings(folder, Uuid::new_v4(), 1, resumed)
        .unwrap();
    tablet
        .request_link_settings(
            folder,
            Uuid::new_v4(),
            1,
            FolderLinkSettings {
                deletion_policy: policy,
                ..resumed
            },
        )
        .unwrap();
    let phone_pending = phone.summaries().unwrap()[0]
        .link_settings
        .as_ref()
        .unwrap()
        .pending_change
        .clone()
        .unwrap();
    let tablet_pending = tablet.summaries().unwrap()[0]
        .link_settings
        .as_ref()
        .unwrap()
        .pending_change
        .clone()
        .unwrap();
    let phone_reply = source
        .receive_link_settings_request(&phone_pending)
        .unwrap();
    let stale_reply = source
        .receive_link_settings_request(&tablet_pending)
        .unwrap();
    assert_eq!(phone_reply.revision, 2);
    assert_eq!(stale_reply.revision, 2);
    assert_eq!(stale_reply.settings, resumed);
    phone.receive_link_settings_commit(&phone_reply).unwrap();
    tablet.receive_link_settings_commit(&stale_reply).unwrap();
    let tablet_state = tablet.summaries().unwrap()[0]
        .link_settings
        .clone()
        .unwrap();
    assert!(tablet_state.pending_change.is_none());
    assert_eq!(tablet_state.conflicted_change, Some(tablet_pending));
    let mut rollback = committed.clone();
    rollback.target_id = c.engine.device_id();
    tablet.receive_link_settings_commit(&rollback).unwrap();
    assert_eq!(
        tablet.summaries().unwrap()[0]
            .link_settings
            .as_ref()
            .unwrap()
            .revision,
        2
    );
    let source_settings = source.desired_settings().unwrap();
    assert_eq!(source_settings.folders[0].members().len(), 3);
    assert_eq!(
        phone.desired_settings().unwrap().folders[0].role(),
        EngineFolderRole::Destination {
            propagate_source_deletions: true,
            restore_local_deletions: true,
        }
    );
    drop(source);
    drop(tablet);
    let source = a.reopen();
    let tablet = c.reopen();
    assert_eq!(
        source.summaries().unwrap()[0]
            .link_settings
            .as_ref()
            .unwrap()
            .revision,
        2
    );
    assert!(
        tablet.summaries().unwrap()[0]
            .link_settings
            .as_ref()
            .unwrap()
            .conflicted_change
            .is_some()
    );
}

#[test]
fn one_destination_confirmation_and_both_signatures_precede_any_membership() {
    let a = Device::new("Mac", 43211);
    let b = Device::new("Docker", 43212);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Photos",
            &a.files(),
            2000,
        )
        .unwrap();
    let revision = first.revision();
    assert_eq!(
        first.summaries().unwrap()[0].expires_at_unix_ms,
        Some(offer.expires_at_unix_ms)
    );
    assert_eq!(
        first
            .offer(
                b.engine.device_id(),
                offer.folder_id,
                "Photos",
                &a.files(),
                2001
            )
            .unwrap(),
        offer
    );
    assert_eq!(first.revision(), revision);
    assert!(first.desired_settings().unwrap().folders.is_empty());
    second.receive_offer(offer.clone(), 2001).unwrap();
    assert!(second.snapshot.shares[0].root.is_none());
    assert!(second.desired_settings().unwrap().folders.is_empty());
    let acceptance = second.accept(offer.offer_id, &b.files(), 2002).unwrap();
    assert_eq!(second.summaries().unwrap()[0].expires_at_unix_ms, None);
    assert!(second.desired_settings().unwrap().folders.is_empty());
    drop(second);
    let mut second = b.reopen();
    assert_eq!(
        second.summaries().unwrap()[0].phase,
        SharingPhase::AwaitingCommit
    );
    let commit = first
        .receive_acceptance(offer.offer_id, acceptance, 2003)
        .unwrap();
    assert_eq!(first.desired_settings().unwrap().folders.len(), 1);
    assert!(second.desired_settings().unwrap().folders.is_empty());
    second.receive_commit(offer.offer_id, commit).unwrap();
    let settings = second.desired_settings().unwrap();
    assert_eq!(settings.folders[0].id(), offer.folder_id);
    assert_eq!(settings.folders[0].root(), b.files());
    assert_eq!(settings.folders[0].members().len(), 2);
    assert_eq!(settings.peers[0].id(), a.installation.device_id());
}

#[test]
fn replay_after_expiry_is_exact_and_removal_survives_repairing() {
    let a = Device::new("Mac", 43221);
    let b = Device::new("Docker", 43222);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, acceptance, commit) = share(&a, &b, &mut first, &mut second);
    let late = 2000 + INVITATION_LIFETIME_MS + 1;
    second.receive_offer(offer.clone(), late).unwrap();
    assert_eq!(
        second.accept(offer.offer_id, &b.files(), late).unwrap(),
        acceptance
    );
    assert_eq!(
        first
            .receive_acceptance(offer.offer_id, acceptance.clone(), late)
            .unwrap(),
        commit
    );
    second.remove(offer.offer_id).unwrap();
    second.remove(offer.offer_id).unwrap();
    drop(second);
    pair(&a, &b);
    let mut second = b.reopen();
    assert_eq!(
        second.receive_offer(offer.clone(), late).unwrap_err(),
        SharingError::Removed
    );
    assert_eq!(
        second.receive_commit(offer.offer_id, commit).unwrap_err(),
        SharingError::Removed
    );
    assert!(second.desired_settings().unwrap().folders.is_empty());
    assert!(b.files().is_dir());
}

#[test]
fn expired_offer_renewal_is_idempotent_replayable_and_rejects_stale_acceptance() {
    let a = Device::new("Mac", 43223);
    let b = Device::new("Docker", 43224);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &a.files(),
            2000,
        )
        .unwrap();
    second.receive_offer(offer.clone(), 2001).unwrap();
    let stale_acceptance = second
        .accept(offer.offer_id, &b.files(), offer.expires_at_unix_ms - 1)
        .unwrap();
    second.set_paused(offer.offer_id, true).unwrap();
    assert_eq!(second.summaries().unwrap()[0].phase, SharingPhase::Paused);
    let renewed = first
        .renew_offer(offer.offer_id, offer.expires_at_unix_ms)
        .unwrap();
    assert_ne!(renewed.offer_id, offer.offer_id);
    assert_eq!(renewed.folder_id, offer.folder_id);
    assert_eq!(renewed.label, offer.label);
    assert_eq!(renewed.source_engine, offer.source_engine);
    assert_eq!(renewed.pairing_id, offer.pairing_id);
    assert!(renewed.issued_at_unix_ms > offer.issued_at_unix_ms);
    assert!(renewed.expires_at_unix_ms > offer.expires_at_unix_ms);
    let renewed_revision = first.revision();
    assert_eq!(
        first
            .renew_offer(offer.offer_id, offer.expires_at_unix_ms + 1)
            .unwrap(),
        renewed
    );
    assert_eq!(first.revision(), renewed_revision);
    assert!(matches!(
        &first.outbound_records().unwrap()[0].record,
        FolderShareRecord::Offer(current) if current == &renewed
    ));

    second
        .receive_offer(renewed.clone(), offer.expires_at_unix_ms + 1)
        .unwrap();
    assert_eq!(
        second
            .renew_offer(offer.offer_id, offer.expires_at_unix_ms + 1)
            .unwrap_err(),
        SharingError::InvalidRecord
    );
    assert_eq!(second.summaries().unwrap().len(), 1);
    assert_eq!(second.summaries().unwrap()[0].offer_id, renewed.offer_id);
    assert_eq!(second.summaries().unwrap()[0].phase, SharingPhase::Offered);
    assert!(second.snapshot.shares[0].root.is_none());
    assert!(second.snapshot.shares[0].acceptance.is_none());
    assert!(second.desired_settings().unwrap().folders.is_empty());
    assert!(second.outbound_records().unwrap().is_empty());
    assert_eq!(
        second
            .receive_offer(offer.clone(), offer.expires_at_unix_ms + 1)
            .unwrap_err(),
        SharingError::Removed
    );
    assert_eq!(
        first
            .receive_acceptance(
                renewed.offer_id,
                stale_acceptance,
                offer.expires_at_unix_ms + 2,
            )
            .unwrap_err(),
        SharingError::InvalidRecord
    );
    let acceptance = second
        .accept(renewed.offer_id, &b.files(), offer.expires_at_unix_ms + 2)
        .unwrap();
    let commit = first
        .receive_acceptance(renewed.offer_id, acceptance, offer.expires_at_unix_ms + 3)
        .unwrap();
    second.receive_commit(renewed.offer_id, commit).unwrap();
    drop(first);
    drop(second);

    let first = a.reopen();
    let second = b.reopen();
    assert_eq!(first.snapshot.shares[0].superseded_offers.len(), 1);
    assert_eq!(second.snapshot.shares[0].superseded_offers.len(), 1);
    for summary in [
        first.summaries().unwrap()[0].clone(),
        second.summaries().unwrap()[0].clone(),
    ] {
        assert_eq!(summary.offer_id, renewed.offer_id);
        assert_eq!(summary.superseded_offer_ids, [offer.offer_id]);
        let encoded = serde_json::to_string(&summary).unwrap();
        assert!(!encoded.contains(b.files().to_str().unwrap()));
        assert!(!encoded.contains(&offer.signature));
    }
    assert_eq!(
        first.index(offer.offer_id).unwrap_err(),
        SharingError::Removed
    );
    assert_eq!(
        second.index(offer.offer_id).unwrap_err(),
        SharingError::Removed
    );
    assert_eq!(first.summaries().unwrap()[0].phase, SharingPhase::Ready);
    assert_eq!(second.summaries().unwrap()[0].phase, SharingPhase::Ready);
}

#[test]
fn renewal_rejects_live_incoming_accepted_and_removed_offers_without_mutation() {
    let a = Device::new("Mac", 43225);
    let b = Device::new("Docker", 43226);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &a.files(),
            2000,
        )
        .unwrap();
    let first_revision = first.revision();
    assert_eq!(
        first
            .renew_offer(offer.offer_id, offer.expires_at_unix_ms - 1)
            .unwrap_err(),
        SharingError::InvalidRecord
    );
    assert_eq!(first.revision(), first_revision);
    second.receive_offer(offer.clone(), 2001).unwrap();
    let second_revision = second.revision();
    assert_eq!(
        second
            .renew_offer(offer.offer_id, offer.expires_at_unix_ms)
            .unwrap_err(),
        SharingError::InvalidRecord
    );
    assert_eq!(second.revision(), second_revision);

    let acceptance = second.accept(offer.offer_id, &b.files(), 2002).unwrap();
    first
        .receive_acceptance(offer.offer_id, acceptance, 2003)
        .unwrap();
    let accepted_revision = first.revision();
    assert_eq!(
        first
            .renew_offer(offer.offer_id, offer.expires_at_unix_ms)
            .unwrap_err(),
        SharingError::InvalidRecord
    );
    assert_eq!(first.revision(), accepted_revision);

    first.remove(offer.offer_id).unwrap();
    let removed_revision = first.revision();
    assert_eq!(
        first
            .renew_offer(offer.offer_id, offer.expires_at_unix_ms)
            .unwrap_err(),
        SharingError::Removed
    );
    assert_eq!(first.revision(), removed_revision);
}

#[test]
fn removed_recipient_is_not_recreated_by_source_renewal() {
    let a = Device::new("Mac", 43227);
    let b = Device::new("Docker", 43228);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &a.files(),
            2000,
        )
        .unwrap();
    second.receive_offer(offer.clone(), 2001).unwrap();
    second.remove(offer.offer_id).unwrap();
    let renewed = first
        .renew_offer(offer.offer_id, offer.expires_at_unix_ms)
        .unwrap();
    let revision = second.revision();
    assert_eq!(
        second
            .receive_offer(renewed, offer.expires_at_unix_ms + 1)
            .unwrap_err(),
        SharingError::Removed
    );
    assert_eq!(second.revision(), revision);
    assert_eq!(second.summaries().unwrap()[0].phase, SharingPhase::Removed);
}

#[test]
fn renewal_quota_accepts_the_last_slot_and_rejects_the_next_without_mutation() {
    let a = Device::new("Mac", 43229);
    let b = Device::new("Docker", 43230);
    pair(&a, &b);
    let mut first = a.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &a.files(),
            2000,
        )
        .unwrap();
    let mut candidate = first.snapshot.clone();
    candidate.shares[0].superseded_offers = (1..MAX_RENEWALS_PER_SHARE)
        .map(|value| SupersededOffer {
            offer_id: Uuid::from_u128(value as u128),
            issued_at_unix_ms: value as u64,
        })
        .collect();
    first.persist(candidate).unwrap();
    let renewed = first
        .renew_offer(offer.offer_id, offer.expires_at_unix_ms)
        .unwrap();
    assert_eq!(
        first.snapshot.shares[0].superseded_offers.len(),
        MAX_RENEWALS_PER_SHARE
    );
    let revision = first.revision();
    assert_eq!(
        first
            .renew_offer(renewed.offer_id, renewed.expires_at_unix_ms)
            .unwrap_err(),
        SharingError::LimitExceeded
    );
    assert_eq!(first.revision(), revision);
    first.remove(renewed.offer_id).unwrap();
    let records = first.outbound_records().unwrap();
    let FolderShareRecord::Removal(notice) = &records[0].record else {
        panic!("expected removal notice");
    };
    assert_eq!(notice.offer_ids.len(), MAX_REMOVAL_OFFER_IDS);
    drop(first);
    let reopened = a.reopen();
    assert_eq!(reopened.snapshot.tombstones.len(), MAX_REMOVAL_OFFER_IDS);
    assert_eq!(
        reopened.snapshot.pending_remote_removals[0].offer_ids.len(),
        MAX_REMOVAL_OFFER_IDS
    );
    let summaries = reopened.summaries().unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].superseded_offer_ids.len(),
        MAX_RENEWALS_PER_SHARE
    );
}

#[test]
fn revocation_is_reconciled_durably_before_restart_settings() {
    let a = Device::new("Mac", 43231);
    let b = Device::new("Docker", 43232);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, _, _) = share(&a, &b, &mut first, &mut second);
    drop(first);
    a.engine.revoke_peer(b.engine.device_id()).unwrap();
    let mut first = a.reopen();
    assert_eq!(first.summaries().unwrap()[0].phase, SharingPhase::Removed);
    assert!(first.desired_settings().unwrap().folders.is_empty());
    drop(first);
    pair(&a, &b);
    let mut first = a.reopen();
    assert!(first.desired_settings().unwrap().folders.is_empty());
    assert_eq!(
        first.set_paused(offer.offer_id, false).unwrap_err(),
        SharingError::Removed
    );
}

#[test]
fn pause_and_unshare_remove_membership_without_touching_files() {
    let a = Device::new("Mac", 43241);
    let b = Device::new("Docker", 43242);
    pair(&a, &b);
    std::fs::write(a.files().join("keep.txt"), b"keep").unwrap();
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, _, _) = share(&a, &b, &mut first, &mut second);
    first.set_paused(offer.offer_id, true).unwrap();
    let settings = first.desired_settings().unwrap();
    assert!(settings.folders.is_empty());
    assert!(settings.peers.is_empty());
    assert!(settings.listener.is_none());
    first.set_paused(offer.offer_id, false).unwrap();
    assert_eq!(first.desired_settings().unwrap().folders.len(), 1);
    first.remove_peer(b.engine.device_id()).unwrap();
    assert!(first.desired_settings().unwrap().folders.is_empty());
    assert_eq!(std::fs::read(a.files().join("keep.txt")).unwrap(), b"keep");
}

#[test]
fn wrong_peer_tampered_records_and_overlapping_roots_never_add_authority() {
    let a = Device::new("Mac", 43251);
    let b = Device::new("Docker", 43252);
    let c = Device::new("Other", 43253);
    pair(&a, &b);
    pair(&a, &c);
    let mut first = a.journal();
    let mut second = b.journal();
    let mut third = c.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &a.files(),
            2000,
        )
        .unwrap();
    assert_eq!(
        third.receive_offer(offer.clone(), 2001).unwrap_err(),
        SharingError::InvalidRecord
    );
    let mut tampered = offer.clone();
    tampered.label = "Changed".into();
    assert_eq!(
        second.receive_offer(tampered, 2001).unwrap_err(),
        SharingError::InvalidRecord
    );
    assert!(second.summaries().unwrap().is_empty());
    let nested = a.files().join("nested");
    std::fs::create_dir(&nested).unwrap();
    assert_eq!(
        first
            .offer(
                c.engine.device_id(),
                Uuid::new_v4(),
                "Nested",
                &nested,
                2002
            )
            .unwrap_err(),
        SharingError::FolderUnavailable
    );
    assert_eq!(first.summaries().unwrap().len(), 1);
}

#[test]
fn replacing_selected_directory_refuses_new_worker_settings() {
    let a = Device::new("Mac", 43261);
    let b = Device::new("Docker", 43262);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    share(&a, &b, &mut first, &mut second);
    std::fs::rename(a.files(), a.root.join("original-files")).unwrap();
    std::fs::create_dir(a.files()).unwrap();
    assert!(matches!(
        first.desired_settings(),
        Err(SharingError::FolderUnavailable)
    ));
    assert!(a.root.join("original-files").is_dir());
}

#[test]
fn stale_journal_cannot_return_a_signature_for_unpersisted_selection() {
    let a = Device::new("Mac", 43271);
    let b = Device::new("Docker", 43272);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Docs",
            &a.files(),
            2000,
        )
        .unwrap();
    second.receive_offer(offer.clone(), 2001).unwrap();
    let mut stale = b.reopen();
    second.remove(offer.offer_id).unwrap();
    assert_eq!(
        stale.accept(offer.offer_id, &b.files(), 2002).unwrap_err(),
        SharingError::PersistenceUncertain
    );
    assert_eq!(
        b.reopen().summaries().unwrap()[0].phase,
        SharingPhase::Removed
    );
}

fn establish_batch_link(
    source_device: &Device,
    target_device: &Device,
    source: &mut FolderSharingJournal,
    target: &mut FolderSharingJournal,
    folder_id: Uuid,
    settings: FolderLinkSettings,
) -> Uuid {
    let offer = source
        .offer_with_settings(
            target_device.engine.device_id(),
            folder_id,
            "Batch",
            &source_device.files(),
            20_000,
            settings,
        )
        .unwrap();
    target.receive_offer(offer.clone(), 20_001).unwrap();
    let acceptance = target
        .accept(offer.offer_id, &target_device.files(), 20_002)
        .unwrap();
    let commit = source
        .receive_acceptance(offer.offer_id, acceptance, 20_003)
        .unwrap();
    target.receive_commit(offer.offer_id, commit).unwrap();
    deliver_link_settings(source, target);
    offer.offer_id
}

fn batch_settings(cadence: LinkCadence) -> FolderLinkSettings {
    FolderLinkSettings {
        deletion_policy: covalent_protocol::FolderLinkPolicy::default(),
        paused: false,
        cadence,
        android_conditions: AndroidLinkConditions::default(),
    }
}

#[test]
fn scheduled_cadence_uses_the_public_camel_case_shape() {
    let cadence = LinkCadence::Scheduled {
        interval_minutes: 30,
    };
    let encoded = serde_json::to_value(cadence).unwrap();
    assert_eq!(
        encoded,
        serde_json::json!({"mode": "scheduled", "intervalMinutes": 30})
    );
    assert_eq!(
        serde_json::from_value::<LinkCadence>(encoded).unwrap(),
        cadence
    );
}

fn index_snapshot(sequence: u64) -> EngineIndexSnapshot {
    EngineIndexSnapshot {
        index_id: "0x0123456789ABCDEF".into(),
        sequence,
    }
}

#[test]
fn manual_run_is_durable_idempotent_and_stops_target_after_local_report() {
    let a = Device::new("Source", 43301);
    let b = Device::new("Target", 43302);
    pair(&a, &b);
    let mut source = a.journal();
    let mut target = b.journal();
    let folder = Uuid::new_v4();
    establish_batch_link(
        &a,
        &b,
        &mut source,
        &mut target,
        folder,
        batch_settings(LinkCadence::Manual),
    );
    assert_eq!(target.summaries().unwrap()[0].phase, SharingPhase::Ready);
    assert!(source.desired_settings().unwrap().folders.is_empty());
    assert!(target.desired_settings().unwrap().folders.is_empty());

    let request_id = Uuid::new_v4();
    assert!(matches!(
        target
            .request_link_run(folder, request_id, 0, 0, 21_000)
            .unwrap(),
        LinkRunAdmission::Pending(_)
    ));
    let pending = target.summaries().unwrap()[0].link_run.clone().unwrap();
    assert_eq!(pending.phase, None);
    assert!(pending.pending_request.is_some());
    let request = target
        .outbound_records()
        .unwrap()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::RunRequest(request) => Some(request),
            _ => None,
        })
        .unwrap();
    let initial = source.receive_link_run_request(&request, 21_001).unwrap();
    assert_eq!(initial.state.as_ref().unwrap().generation, 1);
    assert_eq!(
        source.receive_link_run_request(&request, 21_002).unwrap(),
        initial
    );

    source
        .start_link_run(folder, 1, index_snapshot(8), 21_003)
        .unwrap();
    let running = source
        .outbound_records()
        .unwrap()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::RunCommit(commit) => Some(commit),
            _ => None,
        })
        .unwrap();
    target.receive_link_run_commit(&running).unwrap();
    drop(target);
    let mut target = b.reopen();
    assert!(matches!(
        target
            .request_link_run(folder, request_id, 0, 0, 21_004)
            .unwrap(),
        LinkRunAdmission::AlreadyRunning(state) if state.generation == 1
    ));
    assert_eq!(
        source
            .active_batch_generations(21_004, Instant::now())
            .unwrap()[&folder],
        1
    );
    assert_eq!(
        target
            .active_batch_generations(21_004, Instant::now())
            .unwrap()[&folder],
        1
    );
    let second_request_id = Uuid::new_v4();
    target
        .request_link_run(folder, second_request_id, 1, 0, 21_004)
        .unwrap();
    let second_request = target
        .outbound_records()
        .unwrap()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::RunRequest(request) if request.request_id == second_request_id => {
                Some(request)
            }
            _ => None,
        })
        .unwrap();
    let already_running = source
        .receive_link_run_request(&second_request, 21_004)
        .unwrap();
    assert_eq!(
        already_running.acknowledged_request_id,
        Some(second_request_id)
    );
    target.receive_link_run_commit(&already_running).unwrap();
    assert!(
        target.summaries().unwrap()[0]
            .link_run
            .as_ref()
            .unwrap()
            .pending_request
            .is_none()
    );

    let report = target
        .complete_link_run(
            folder,
            1,
            index_snapshot(8),
            LinkRunDestinationResult::Succeeded,
            21_005,
        )
        .unwrap();
    assert!(
        target
            .active_batch_generations(21_006, Instant::now())
            .unwrap()
            .is_empty()
    );
    let finished = source.receive_link_run_report(&report, 21_006).unwrap();
    assert_eq!(finished.phase, LinkRunPhase::Succeeded);
    assert_eq!(
        source.receive_link_run_report(&report, 21_007).unwrap(),
        finished
    );
    let final_commit = source.receive_link_run_request(&request, 21_008).unwrap();
    assert_eq!(
        final_commit.state.as_ref().unwrap().phase,
        LinkRunPhase::Succeeded
    );
    target.receive_link_run_commit(&final_commit).unwrap();
    assert!(matches!(
        target
            .request_link_run(folder, request_id, 0, 0, 21_009)
            .unwrap(),
        LinkRunAdmission::AlreadyRunning(state) if state.phase == LinkRunPhase::Succeeded
    ));
    drop(source);
    drop(target);
    assert_eq!(
        a.reopen().summaries().unwrap()[0]
            .link_run
            .as_ref()
            .unwrap()
            .phase,
        Some(LinkRunPhase::Succeeded)
    );
    assert_eq!(
        b.reopen().summaries().unwrap()[0]
            .link_run
            .as_ref()
            .unwrap()
            .phase,
        Some(LinkRunPhase::Succeeded)
    );
}

#[test]
fn source_rejects_stale_first_request_without_inventing_a_run() {
    let a = Device::new("Source", 43311);
    let b = Device::new("Target", 43312);
    pair(&a, &b);
    let mut source = a.journal();
    let mut target = b.journal();
    let folder = Uuid::new_v4();
    establish_batch_link(
        &a,
        &b,
        &mut source,
        &mut target,
        folder,
        batch_settings(LinkCadence::Manual),
    );
    source
        .request_link_settings_at(
            folder,
            Uuid::new_v4(),
            0,
            FolderLinkSettings {
                android_conditions: AndroidLinkConditions {
                    wifi_only: true,
                    charging_only: false,
                },
                ..batch_settings(LinkCadence::Manual)
            },
            21_900,
        )
        .unwrap();
    target
        .request_link_run(folder, Uuid::new_v4(), 0, 0, 22_000)
        .unwrap();
    let request = target
        .outbound_records()
        .unwrap()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::RunRequest(request) => Some(request),
            _ => None,
        })
        .unwrap();
    let reply = source.receive_link_run_request(&request, 22_000).unwrap();
    assert!(reply.state.is_none());
    assert_eq!(
        reply.rejected_request.as_ref().unwrap().reason,
        LinkRunRejectionReason::SettingsChanged
    );
    target.receive_link_run_commit(&reply).unwrap();
    let summary = target.summaries().unwrap()[0].link_run.clone().unwrap();
    assert_eq!(summary.generation, 0);
    assert_eq!(summary.phase, None);
    assert_eq!(summary.rejected_request, reply.rejected_request);
}

#[test]
fn scheduled_admission_uses_acceptance_then_last_end_and_source_restart_interrupts() {
    let a = Device::new("Source", 43321);
    let b = Device::new("Target", 43322);
    pair(&a, &b);
    let mut source = a.journal();
    let mut target = b.journal();
    let folder = Uuid::new_v4();
    establish_batch_link(
        &a,
        &b,
        &mut source,
        &mut target,
        folder,
        batch_settings(LinkCadence::Scheduled {
            interval_minutes: 15,
        }),
    );
    let initial = source.summaries().unwrap()[0].link_run.clone().unwrap();
    assert_eq!(initial.generation, 0);
    assert_eq!(initial.phase, None);
    assert_eq!(initial.next_due_at_unix_ms, Some(920_000));
    assert!(source.admit_due_link_runs(919_999).unwrap().is_empty());
    assert_eq!(source.admit_due_link_runs(920_000).unwrap().len(), 1);
    assert!(source.admit_due_link_runs(920_001).unwrap().is_empty());
    source
        .start_link_run(folder, 1, index_snapshot(2), 920_002)
        .unwrap();
    drop(source);
    let mut reopened = a.reopen();
    assert!(reopened.interrupt_unfinished_link_runs(920_003).unwrap());
    let summary = reopened.summaries().unwrap()[0].link_run.clone().unwrap();
    assert_eq!(summary.generation, 1);
    assert_eq!(summary.phase, Some(LinkRunPhase::Interrupted));
    assert_eq!(summary.next_due_at_unix_ms, Some(1_820_003));
}

#[test]
fn settings_change_cancels_only_current_batch_work() {
    let a = Device::new("Source", 43331);
    let b = Device::new("Target", 43332);
    pair(&a, &b);
    let mut source = a.journal();
    let mut target = b.journal();
    let folder = Uuid::new_v4();
    establish_batch_link(
        &a,
        &b,
        &mut source,
        &mut target,
        folder,
        batch_settings(LinkCadence::Manual),
    );
    assert!(matches!(
        source
            .request_link_run(folder, Uuid::new_v4(), 0, 0, 23_000)
            .unwrap(),
        LinkRunAdmission::Accepted(_)
    ));
    source
        .request_link_settings_at(
            folder,
            Uuid::new_v4(),
            0,
            FolderLinkSettings {
                paused: true,
                ..batch_settings(LinkCadence::Manual)
            },
            23_001,
        )
        .unwrap();
    let summary = source.summaries().unwrap()[0].link_run.clone().unwrap();
    assert_eq!(summary.generation, 1);
    assert_eq!(summary.phase, Some(LinkRunPhase::Cancelled));
    assert!(
        source
            .active_batch_generations(23_002, Instant::now())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn removed_destination_does_not_hold_a_fanout_run_open() {
    let a = Device::new("Source", 43341);
    let b = Device::new("First target", 43342);
    let c = Device::new("Second target", 43343);
    pair(&a, &b);
    pair(&a, &c);
    let mut source = a.journal();
    let mut first = b.journal();
    let mut second = c.journal();
    let folder = Uuid::new_v4();
    let first_offer = establish_batch_link(
        &a,
        &b,
        &mut source,
        &mut first,
        folder,
        batch_settings(LinkCadence::Manual),
    );
    establish_batch_link(
        &a,
        &c,
        &mut source,
        &mut second,
        folder,
        batch_settings(LinkCadence::Manual),
    );
    let state = match source
        .request_link_run(folder, Uuid::new_v4(), 0, 0, 24_000)
        .unwrap()
    {
        LinkRunAdmission::Accepted(state) => state,
        other => panic!("unexpected admission: {other:?}"),
    };
    assert_eq!(state.destinations.len(), 2);
    source.remove(first_offer).unwrap();
    let summary = source
        .summaries()
        .unwrap()
        .into_iter()
        .find(|item| item.peer_id == c.engine.device_id())
        .unwrap()
        .link_run
        .unwrap();
    assert_eq!(summary.phase, Some(LinkRunPhase::Preparing));
    assert_eq!(
        summary
            .destinations
            .iter()
            .find(|item| item.peer_id == b.engine.device_id())
            .unwrap()
            .result,
        LinkRunDestinationResult::Cancelled
    );
}

#[test]
fn android_batch_membership_requires_a_fresh_local_condition_observation() {
    let a = Device::new("Source", 43351);
    let b = Device::new("Android target", 43352);
    pair(&a, &b);
    let mut source = a.journal();
    let mut target = b.journal();
    let folder = Uuid::new_v4();
    establish_batch_link(
        &a,
        &b,
        &mut source,
        &mut target,
        folder,
        FolderLinkSettings {
            android_conditions: AndroidLinkConditions {
                wifi_only: true,
                charging_only: true,
            },
            ..batch_settings(LinkCadence::Manual)
        },
    );
    let request_id = Uuid::new_v4();
    let state = match source
        .request_link_run(folder, request_id, 0, 0, 25_000)
        .unwrap()
    {
        LinkRunAdmission::Accepted(state) => state,
        other => panic!("unexpected admission: {other:?}"),
    };
    source
        .start_link_run(folder, state.generation, index_snapshot(4), 25_001)
        .unwrap();
    let commit = source
        .outbound_records()
        .unwrap()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::RunCommit(commit) => Some(commit),
            _ => None,
        })
        .unwrap();
    target.receive_link_run_commit(&commit).unwrap();
    target.set_android_host(true);
    let observed_at = Instant::now();
    let (desired, runs) = target
        .desired_settings_and_runs(25_002, observed_at)
        .unwrap();
    assert!(desired.folders.is_empty());
    assert!(runs.is_empty());
    target.observe_android_conditions(true, false, observed_at);
    assert!(
        target
            .active_batch_generations(25_002, observed_at)
            .unwrap()
            .is_empty()
    );
    target.observe_android_conditions(true, true, observed_at);
    let (desired, runs) = target
        .desired_settings_and_runs(25_002, observed_at)
        .unwrap();
    assert_eq!(desired.folders.len(), 1);
    assert_eq!(runs[&folder], 1);
    let (desired, runs) = target
        .desired_settings_and_runs(25_002, observed_at + Duration::from_secs(91))
        .unwrap();
    assert!(desired.folders.is_empty());
    assert!(runs.is_empty());
}

#[test]
fn continuous_android_link_uses_the_same_local_condition_gate() {
    let a = Device::new("Source", 43356);
    let b = Device::new("Android target", 43357);
    pair(&a, &b);
    let mut source = a.journal();
    let mut target = b.journal();
    let folder = Uuid::new_v4();
    establish_batch_link(
        &a,
        &b,
        &mut source,
        &mut target,
        folder,
        FolderLinkSettings {
            cadence: LinkCadence::Continuous,
            android_conditions: AndroidLinkConditions {
                wifi_only: true,
                charging_only: false,
            },
            ..batch_settings(LinkCadence::Continuous)
        },
    );
    target.set_android_host(true);
    assert_eq!(target.summaries().unwrap()[0].phase, SharingPhase::Ready);
    assert!(target.summaries().unwrap()[0].waiting_for_conditions);
    assert!(!source.summaries().unwrap()[0].waiting_for_conditions);
    assert!(target.desired_settings().unwrap().folders.is_empty());
    target.observe_android_conditions(true, false, Instant::now());
    assert_eq!(target.desired_settings().unwrap().folders.len(), 1);
    assert!(!target.summaries().unwrap()[0].waiting_for_conditions);
    target.observe_android_conditions(false, true, Instant::now());
    assert!(target.desired_settings().unwrap().folders.is_empty());
    assert!(target.summaries().unwrap()[0].waiting_for_conditions);
    target.observe_android_conditions(
        true,
        true,
        Instant::now() - std::time::Duration::from_secs(91),
    );
    assert!(target.summaries().unwrap()[0].waiting_for_conditions);
    target.set_android_host(false);
    assert!(!target.summaries().unwrap()[0].waiting_for_conditions);
}

#[test]
fn deadline_finishes_offline_destinations_without_claiming_success() {
    let a = Device::new("Source", 43361);
    let b = Device::new("Offline target", 43362);
    pair(&a, &b);
    let mut source = a.journal();
    let mut target = b.journal();
    let folder = Uuid::new_v4();
    establish_batch_link(
        &a,
        &b,
        &mut source,
        &mut target,
        folder,
        batch_settings(LinkCadence::Manual),
    );
    let started_at = 26_000;
    source
        .request_link_run(folder, Uuid::new_v4(), 0, 0, started_at)
        .unwrap();
    assert!(
        !source
            .expire_link_runs(started_at + LINK_RUN_DEADLINE_MS - 1)
            .unwrap()
    );
    assert!(
        source
            .expire_link_runs(started_at + LINK_RUN_DEADLINE_MS + 50)
            .unwrap()
    );
    let summary = source.summaries().unwrap()[0].link_run.clone().unwrap();
    assert_eq!(summary.phase, Some(LinkRunPhase::Incomplete));
    assert_eq!(
        summary.ended_at_unix_ms,
        Some(started_at + LINK_RUN_DEADLINE_MS)
    );
    assert_eq!(
        summary.destinations[0].result,
        LinkRunDestinationResult::TimedOut
    );
}

#[test]
fn same_identity_repairing_between_reconciliations_does_not_restore_share() {
    let a = Device::new("Mac", 43311);
    let b = Device::new("Docker", 43312);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    share(&a, &b, &mut first, &mut second);
    a.engine.revoke_peer(b.engine.device_id()).unwrap();
    pair_at(&a, &b, 5000);
    assert!(first.desired_settings().unwrap().folders.is_empty());
    assert_eq!(first.summaries().unwrap()[0].phase, SharingPhase::Removed);
    let mut config = a.engine.config().unwrap();
    config
        .trusted_peers
        .get_mut(&b.engine.device_id())
        .unwrap()
        .public_key = "damaged".into();
    assert!(matches!(
        observe_trust(&config, b.engine.device_id()),
        Err(SharingError::InvalidState)
    ));
}

#[test]
fn one_folder_accepts_multiple_peers_but_rejects_inconsistent_labels_before_persistence() {
    let a = Device::new("Mac", 43321);
    let b = Device::new("Docker", 43322);
    let c = Device::new("Phone", 43323);
    pair(&a, &b);
    pair(&a, &c);
    let mut first = a.journal();
    let mut second = b.journal();
    let mut third = c.journal();
    let (original, _, _) = share(&a, &b, &mut first, &mut second);
    let before = first.store.revision();
    assert_eq!(
        first
            .offer(
                c.engine.device_id(),
                original.folder_id,
                "Other label",
                &a.files(),
                3000
            )
            .unwrap_err(),
        SharingError::InvalidRecord
    );
    assert_eq!(first.store.revision(), before);
    let offer = first
        .offer(
            c.engine.device_id(),
            original.folder_id,
            "Documents",
            &a.files(),
            3000,
        )
        .unwrap();
    third.receive_offer(offer.clone(), 3001).unwrap();
    let accepted = third.accept(offer.offer_id, &c.files(), 3002).unwrap();
    let committed = first
        .receive_acceptance(offer.offer_id, accepted, 3003)
        .unwrap();
    third.receive_commit(offer.offer_id, committed).unwrap();
    let settings = first.desired_settings().unwrap();
    assert_eq!(settings.folders.len(), 1);
    assert_eq!(settings.folders[0].members().len(), 3);
    assert_eq!(settings.peers.len(), 2);
    first.set_paused(original.offer_id, true).unwrap();
    let settings = first.desired_settings().unwrap();
    assert_eq!(settings.folders[0].members().len(), 2);
    assert_eq!(settings.peers[0].id(), c.installation.device_id());
}

#[test]
fn pending_peer_quota_preserves_capacity_and_removed_history_is_compact() {
    let a = Device::new("Mac", 43331);
    let b = Device::new("Docker", 43332);
    pair(&a, &b);
    let first = a.journal();
    let mut second = b.journal();
    let mut first_offer = None;
    for n in 0..MAX_PEER_PENDING_OFFERS + 1 {
        let offer = a
            .engine
            .issue_folder_share_offer(
                b.engine.device_id(),
                Uuid::new_v4(),
                "Inbox",
                first.binding().clone(),
                None,
                2000,
                INVITATION_LIFETIME_MS,
            )
            .unwrap();
        if n == 0 {
            first_offer = Some(offer.clone());
        }
        let result = second.receive_offer(offer, 2001);
        if n < MAX_PEER_PENDING_OFFERS {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err(), SharingError::LimitExceeded);
        }
    }
    let offer = first_offer.unwrap();
    second.remove(offer.offer_id).unwrap();
    assert_eq!(second.snapshot.shares.len(), MAX_PEER_PENDING_OFFERS - 1);
    assert_eq!(second.snapshot.tombstones.len(), 1);
    let json = serde_json::to_value(&second.snapshot.tombstones)
        .unwrap()
        .to_string();
    assert!(!json.contains("sourceEngine"));
    assert!(!json.contains("root"));
    let replacement = a
        .engine
        .issue_folder_share_offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Inbox",
            first.binding().clone(),
            None,
            2000,
            INVITATION_LIFETIME_MS,
        )
        .unwrap();
    second.receive_offer(replacement, 2002).unwrap();
}

#[test]
fn canonical_removed_status_survives_freshness_advance_without_resurrection() {
    let a = Device::new("Mac", 43341);
    let b = Device::new("Docker", 43342);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, _, commit) = share(&a, &b, &mut first, &mut second);
    second.remove(offer.offer_id).unwrap();
    second
        .acknowledge_removal(a.engine.device_id(), offer.offer_id)
        .unwrap();
    second
        .advance_freshness_floor(2000 + INVITATION_LIFETIME_MS + 5 * 60 * 1000 + 1)
        .unwrap();
    assert_eq!(second.snapshot.tombstones.len(), 1);
    assert_eq!(second.summaries().unwrap()[0].phase, SharingPhase::Removed);
    drop(second);
    let mut second = b.reopen();
    assert_eq!(
        second.receive_offer(offer.clone(), 2001).unwrap_err(),
        SharingError::Removed
    );
    assert!(second.receive_commit(offer.offer_id, commit).is_err());
    assert!(second.desired_settings().unwrap().folders.is_empty());
}

#[test]
fn wildcard_listener_cannot_advertise_an_unbound_address_family() {
    let a = Device::new("Mac", 43351);
    assert!(matches!(
        FolderSharingJournal::create(
            Arc::clone(&a.engine),
            Arc::clone(&a.installation),
            Arc::clone(&a.protector),
            "0.0.0.0:43351".parse().unwrap(),
            "[::1]:43351".parse().unwrap()
        ),
        Err(SharingError::InvalidState)
    ));
}

#[test]
fn conflicting_signed_engine_bindings_cannot_poison_existing_membership() {
    let a = Device::new("Mac", 43361);
    let b = Device::new("Docker", 43362);
    let c = Device::new("Phone", 43363);
    pair(&a, &b);
    pair(&a, &c);
    let mut first = a.journal();
    let mut second = b.journal();
    share(&a, &b, &mut first, &mut second);
    let original = first.desired_settings().unwrap();

    // Every record is genuinely signed by its paired Covalent owner. Reject
    // conflicting engine ownership before committing any additional authority.
    for (n, target, claimed_engine) in [
        (0, &b, c.installation.device_id()),
        (1, &c, b.installation.device_id()),
        (2, &c, a.installation.device_id()),
    ] {
        let path = a.root.join(format!("other-{n}"));
        std::fs::create_dir(&path).unwrap();
        let offer = first
            .offer(
                target.engine.device_id(),
                Uuid::new_v4(),
                "Other",
                &path,
                2100,
            )
            .unwrap();
        let binding = SyncEngineBinding::new(
            target.engine.device_id(),
            claimed_engine.as_str(),
            &target.address.to_string(),
        )
        .unwrap();
        let acceptance = target
            .engine
            .accept_folder_share(&offer, binding, 2101)
            .unwrap();
        let revision = first.revision();
        assert_eq!(
            first
                .receive_acceptance(offer.offer_id, acceptance, 2102)
                .unwrap_err(),
            SharingError::InvalidRecord
        );
        assert_eq!(first.revision(), revision);
        assert!(first.desired_settings().unwrap() == original);
        first.remove(offer.offer_id).unwrap();
    }
    drop(first);
    assert!(a.reopen().desired_settings().unwrap() == original);
}

#[test]
fn durable_delivery_records_resume_exactly_and_stop_after_removal_or_revocation() {
    let a = Device::new("Mac", 43371);
    let b = Device::new("Docker", 43372);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Photos",
            &a.files(),
            2000,
        )
        .unwrap();
    assert_eq!(
        first.outbound_records().unwrap()[0].record,
        FolderShareRecord::Offer(offer.clone())
    );
    second.receive_offer(offer.clone(), 2001).unwrap();
    assert!(second.outbound_records().unwrap().is_empty());
    let acceptance = second.accept(offer.offer_id, &b.files(), 2002).unwrap();
    drop(second);
    let mut second = b.reopen();
    let outgoing = second.outbound_records().unwrap();
    assert_eq!(outgoing[0].peer_transport, a.transport());
    assert_eq!(
        outgoing[0].record,
        FolderShareRecord::Acceptance {
            offer_id: offer.offer_id,
            acceptance: acceptance.clone()
        }
    );
    let commit = first
        .receive_acceptance(offer.offer_id, acceptance, 2003)
        .unwrap();
    drop(first);
    let mut first = a.reopen();
    assert_eq!(
        first.outbound_records().unwrap()[0].record,
        FolderShareRecord::Commit {
            offer_id: offer.offer_id,
            commit: commit.clone()
        }
    );
    second.receive_commit(offer.offer_id, commit).unwrap();
    assert!(second.outbound_records().unwrap().is_empty());
    first.remove(offer.offer_id).unwrap();
    let outgoing = first.outbound_records().unwrap();
    assert_eq!(outgoing.len(), 1);
    assert!(matches!(
        &outgoing[0].record,
        FolderShareRecord::Removal(notice)
            if notice.requester_id == a.engine.device_id()
                && notice.target_id == b.engine.device_id()
                && notice.offer_source_id == a.engine.device_id()
                && notice.folder_id == offer.folder_id
                && notice.offer_ids == [offer.offer_id]
    ));
    first
        .acknowledge_removal(b.engine.device_id(), offer.offer_id)
        .unwrap();
    assert!(first.outbound_records().unwrap().is_empty());
    b.engine.revoke_peer(a.engine.device_id()).unwrap();
    assert!(second.outbound_records().unwrap().is_empty());
    assert_eq!(second.summaries().unwrap()[0].phase, SharingPhase::Removed);
}

#[test]
fn remote_removal_replays_across_restart_matches_renewal_prefix_and_keeps_files() {
    let a = Device::new("Mac", 43373);
    let b = Device::new("Docker", 43374);
    let c = Device::new("Other", 43375);
    pair(&a, &b);
    pair(&b, &c);
    std::fs::write(a.files().join("source.txt"), b"source bytes").unwrap();
    std::fs::write(b.files().join("target.txt"), b"target bytes").unwrap();
    let mut first = a.journal();
    let mut second = b.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &a.files(),
            2_000,
        )
        .unwrap();
    second.receive_offer(offer.clone(), 2_001).unwrap();
    let first_renewal = first
        .renew_offer(offer.offer_id, offer.expires_at_unix_ms)
        .unwrap();
    let renewed = first
        .renew_offer(first_renewal.offer_id, first_renewal.expires_at_unix_ms)
        .unwrap();
    first.remove(renewed.offer_id).unwrap();
    let summaries = first.summaries().unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].offer_id, renewed.offer_id);
    assert_eq!(
        summaries[0].superseded_offer_ids,
        [offer.offer_id, first_renewal.offer_id]
    );
    assert!(summaries[0].remote_removal_pending);
    drop(first);

    let mut first = a.reopen();
    let records = first.outbound_records().unwrap();
    let notice = match &records[0].record {
        FolderShareRecord::Removal(notice) => notice.clone(),
        other => panic!("expected removal, got {other:?}"),
    };
    assert_eq!(
        notice.offer_ids,
        [offer.offer_id, first_renewal.offer_id, renewed.offer_id]
    );

    let revision = second.revision();
    let mut wrong = notice.clone();
    wrong.folder_id = Uuid::new_v4();
    assert_eq!(
        second.receive_removal(&wrong).unwrap_err(),
        SharingError::InvalidRecord
    );
    wrong = notice.clone();
    wrong.requester_id = c.engine.device_id();
    assert_eq!(
        second.receive_removal(&wrong).unwrap_err(),
        SharingError::InvalidRecord
    );
    wrong = notice.clone();
    wrong.offer_ids[0] = Uuid::new_v4();
    assert_eq!(
        second.receive_removal(&wrong).unwrap_err(),
        SharingError::InvalidRecord
    );
    wrong = notice.clone();
    wrong.offer_ids.push(wrong.offer_ids[0]);
    assert_eq!(
        second.receive_removal(&wrong).unwrap_err(),
        SharingError::InvalidRecord
    );
    wrong = notice.clone();
    wrong.offer_ids.clear();
    assert_eq!(
        second.receive_removal(&wrong).unwrap_err(),
        SharingError::InvalidRecord
    );
    wrong = notice.clone();
    wrong.offer_ids = (1..=MAX_REMOVAL_OFFER_IDS + 1)
        .map(|value| Uuid::from_u128(value as u128))
        .collect();
    assert_eq!(
        second.receive_removal(&wrong).unwrap_err(),
        SharingError::InvalidRecord
    );
    assert_eq!(second.revision(), revision);

    second.receive_removal(&notice).unwrap();
    let removed_revision = second.revision();
    second.receive_removal(&notice).unwrap();
    assert_eq!(second.revision(), removed_revision);
    assert!(second.desired_settings().unwrap().folders.is_empty());
    assert!(second.outbound_records().unwrap().is_empty());
    assert_eq!(
        std::fs::read(a.files().join("source.txt")).unwrap(),
        b"source bytes"
    );
    assert_eq!(
        std::fs::read(b.files().join("target.txt")).unwrap(),
        b"target bytes"
    );

    first
        .acknowledge_removal(b.engine.device_id(), renewed.offer_id)
        .unwrap();
    let acknowledged_revision = first.revision();
    first
        .acknowledge_removal(b.engine.device_id(), renewed.offer_id)
        .unwrap();
    assert_eq!(first.revision(), acknowledged_revision);
    assert!(
        !first.summaries().unwrap().iter().any(|summary| {
            summary.offer_id == renewed.offer_id && summary.remote_removal_pending
        })
    );
    drop(first);
    let mut reopened = a.reopen();
    let current = reopened
        .summaries()
        .unwrap()
        .into_iter()
        .find(|summary| summary.offer_id == renewed.offer_id)
        .unwrap();
    assert_eq!(
        current.superseded_offer_ids,
        [offer.offer_id, first_renewal.offer_id]
    );
    assert!(!current.remote_removal_pending);
    assert!(reopened.outbound_records().unwrap().is_empty());
}

#[test]
fn removal_and_renewal_crossing_in_flight_stop_the_whole_logical_invitation() {
    let a = Device::new("Mac", 43379);
    let b = Device::new("Docker", 43380);
    pair(&a, &b);
    let mut source = a.journal();
    let mut target = b.journal();
    let original = source
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &a.files(),
            2_000,
        )
        .unwrap();
    target.receive_offer(original.clone(), 2_001).unwrap();

    // The source renews while the target is offline. The target removes the
    // older ID it knows, so its authenticated notice is a strict prefix of the
    // source's current chain.
    let renewed = source
        .renew_offer(original.offer_id, original.expires_at_unix_ms)
        .unwrap();
    target.remove(original.offer_id).unwrap();
    let short_notice = match target.outbound_records().unwrap()[0].record.clone() {
        FolderShareRecord::Removal(notice) => notice,
        other => panic!("expected removal, got {other:?}"),
    };
    assert_eq!(short_notice.offer_ids, [original.offer_id]);
    source.receive_removal(&short_notice).unwrap();
    let removed_revision = source.revision();
    source.receive_removal(&short_notice).unwrap();
    assert_eq!(source.revision(), removed_revision);
    let removed = source.summaries().unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].offer_id, renewed.offer_id);
    assert_eq!(removed[0].superseded_offer_ids, [original.offer_id]);
    assert_eq!(removed[0].phase, SharingPhase::Removed);
    assert!(source.desired_settings().unwrap().folders.is_empty());

    // A later full-chain retry is compatible and remains idempotent.
    let mut full_notice = short_notice.clone();
    full_notice.offer_ids.push(renewed.offer_id);
    source.receive_removal(&full_notice).unwrap();
    assert_eq!(source.revision(), removed_revision);
    drop(source);

    // The renewal cannot arrive after the refusal and revive the target, even
    // though the target did not know its new ID when it removed the share.
    drop(target);
    let mut target = b.reopen();
    assert_eq!(
        target
            .receive_offer(renewed.clone(), renewed.issued_at_unix_ms)
            .unwrap_err(),
        SharingError::Removed
    );
    drop(target);
    let mut target = b.reopen();
    assert_eq!(
        target
            .receive_offer(renewed, original.expires_at_unix_ms)
            .unwrap_err(),
        SharingError::Removed
    );

    // If removal wins before a renewal is signed, the source cannot renew the
    // now-terminal invitation either.
    let mut source = a.reopen();
    assert_eq!(
        source
            .renew_offer(original.offer_id, original.expires_at_unix_ms)
            .unwrap_err(),
        SharingError::Removed
    );
}

#[test]
fn removal_overtakes_unseen_offer_and_permanently_blocks_its_delayed_delivery() {
    let a = Device::new("Mac", 43376);
    let b = Device::new("Docker", 43377);
    let c = Device::new("Other", 43378);
    pair(&a, &b);
    pair(&b, &c);
    std::fs::write(a.files().join("source.txt"), b"source remains").unwrap();
    std::fs::write(b.files().join("target.txt"), b"target remains").unwrap();
    let mut first = a.journal();
    let mut second = b.journal();
    let offer = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &a.files(),
            2_000,
        )
        .unwrap();
    first.remove(offer.offer_id).unwrap();
    let later = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Other",
            &a.files(),
            2_100,
        )
        .unwrap();
    let records = first.outbound_records().unwrap();
    assert!(matches!(records[0].record, FolderShareRecord::Removal(_)));
    let notice = records
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::Removal(notice) => Some(notice),
            _ => None,
        })
        .unwrap();

    second.receive_removal(&notice).unwrap();
    let revision = second.revision();
    second.receive_removal(&notice).unwrap();
    assert_eq!(second.revision(), revision);
    assert!(second.summaries().unwrap().is_empty());
    drop(second);

    let mut second = b.reopen();
    assert_eq!(
        second.receive_offer(offer, 2_001).unwrap_err(),
        SharingError::Removed
    );
    let collision = FolderRemovalNotice {
        requester_id: c.engine.device_id(),
        target_id: b.engine.device_id(),
        offer_source_id: c.engine.device_id(),
        folder_id: Uuid::new_v4(),
        offer_ids: notice.offer_ids.clone(),
    };
    assert_eq!(
        second.receive_removal(&collision).unwrap_err(),
        SharingError::InvalidRecord
    );
    assert_eq!(second.revision(), revision);

    first
        .acknowledge_removal(b.engine.device_id(), notice.offer_ids[0])
        .unwrap();
    assert!(matches!(
        first.outbound_records().unwrap()[0].record,
        FolderShareRecord::Offer(ref pending) if pending == &later
    ));
    assert_eq!(
        std::fs::read(a.files().join("source.txt")).unwrap(),
        b"source remains"
    );
    assert_eq!(
        std::fs::read(b.files().join("target.txt")).unwrap(),
        b"target remains"
    );
}

#[test]
fn repaired_root_is_idempotent_durable_and_preserves_signed_records() {
    let a = Device::new("Mac", 43381);
    let b = Device::new("Docker", 43382);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, acceptance, commit) = share(&a, &b, &mut first, &mut second);
    let replacement = a.root.join("replacement");
    std::fs::create_dir(&replacement).unwrap();

    first.repair_root(offer.offer_id, &replacement).unwrap();
    let revision = first.revision();
    first.repair_root(offer.offer_id, &replacement).unwrap();
    assert_eq!(first.revision(), revision);
    assert_eq!(first.snapshot.shares[0].offer, offer);
    assert_eq!(first.snapshot.shares[0].acceptance, Some(acceptance));
    assert_eq!(first.snapshot.shares[0].commit, Some(commit));

    drop(first);
    let mut reopened = a.reopen();
    assert_eq!(
        reopened.desired_settings().unwrap().folders[0].root(),
        replacement
    );
}

#[test]
fn paused_share_can_renew_exact_root_but_cannot_move_it() {
    let a = Device::new("Mac", 43392);
    let b = Device::new("Docker", 43393);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, acceptance, commit) = share(&a, &b, &mut first, &mut second);
    first.set_paused(offer.offer_id, true).unwrap();
    let paused_revision = first.revision();

    first.repair_root(offer.offer_id, &a.files()).unwrap();
    assert_eq!(first.revision(), paused_revision);
    assert!(first.snapshot.shares[0].paused);
    assert!(first.snapshot.pending_root_reset.is_none());
    assert_eq!(first.snapshot.shares[0].offer, offer);
    assert_eq!(first.snapshot.shares[0].acceptance, Some(acceptance));
    assert_eq!(first.snapshot.shares[0].commit, Some(commit));

    let replacement = a.root.join("paused-replacement");
    std::fs::create_dir(&replacement).unwrap();
    assert_eq!(
        first.repair_root(offer.offer_id, &replacement).unwrap_err(),
        SharingError::InvalidState
    );
    assert_eq!(first.revision(), paused_revision);
    assert!(first.snapshot.pending_root_reset.is_none());
    assert!(same_root(
        first.snapshot.shares[0].root.as_ref().unwrap(),
        &capture_root(&a.files(), &a.installation).unwrap()
    ));
}

#[test]
fn changed_root_repair_refuses_implicit_multi_peer_capability() {
    let a = Device::new("Mac", 43383);
    let b = Device::new("Docker", 43384);
    let c = Device::new("Phone", 43385);
    pair(&a, &b);
    pair(&a, &c);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, _, _) = share(&a, &b, &mut first, &mut second);
    first
        .offer(
            c.engine.device_id(),
            offer.folder_id,
            "Documents",
            &a.files(),
            3000,
        )
        .unwrap();
    let replacement = a.root.join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    let revision = first.revision();

    assert_eq!(
        first.repair_root(offer.offer_id, &replacement).unwrap_err(),
        SharingError::AlreadyShared
    );
    assert_eq!(first.revision(), revision);
    assert!(same_root(
        first.snapshot.shares[0].root.as_ref().unwrap(),
        first.snapshot.shares[1].root.as_ref().unwrap()
    ));
}

#[test]
fn root_reset_intent_is_revision_bound_durable_and_revalidates_selection() {
    let a = Device::new("Mac", 43390);
    let b = Device::new("Docker", 43391);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, _, _) = share(&a, &b, &mut first, &mut second);
    let replacement = a.root.join("replacement-reset");
    std::fs::create_dir(&replacement).unwrap();

    let stale = first
        .prepare_root_repair(offer.offer_id, &replacement)
        .unwrap();
    first.set_paused(offer.offer_id, true).unwrap();
    assert_eq!(
        first.commit_root_repair(stale).unwrap_err(),
        SharingError::InvalidState
    );
    first.set_paused(offer.offer_id, false).unwrap();

    let prepared = first
        .prepare_root_repair(offer.offer_id, &replacement)
        .unwrap();
    assert_eq!(
        first.commit_root_repair(prepared).unwrap(),
        Some(offer.folder_id)
    );
    let pending_revision = first.revision();
    drop(first);

    let mut reopened = a.reopen();
    assert_eq!(
        reopened.pending_root_reset().unwrap(),
        Some(offer.folder_id)
    );
    assert_eq!(
        reopened.desired_settings().unwrap().folders[0].root(),
        replacement
    );
    reopened.repair_root(offer.offer_id, &replacement).unwrap();
    assert_eq!(reopened.revision(), pending_revision);

    let competing = a.root.join("competing-reset");
    std::fs::create_dir(&competing).unwrap();
    assert_eq!(
        reopened
            .prepare_root_repair(offer.offer_id, &competing)
            .unwrap_err(),
        SharingError::InvalidState
    );
    assert_eq!(reopened.revision(), pending_revision);

    // Retain the original directory so Linux cannot recycle its inode for
    // the replacement; this test requires a genuinely different identity.
    let retained = a.root.join("retained-reset-selection");
    std::fs::rename(&replacement, &retained).unwrap();
    std::fs::create_dir(&replacement).unwrap();
    let old_metadata = std::fs::symlink_metadata(&retained).unwrap();
    let new_metadata = std::fs::symlink_metadata(&replacement).unwrap();
    assert_ne!(
        (old_metadata.dev(), old_metadata.ino()),
        (new_metadata.dev(), new_metadata.ino())
    );
    assert_eq!(
        reopened.complete_root_reset(offer.folder_id).unwrap_err(),
        SharingError::FolderUnavailable
    );
    assert_eq!(
        reopened.pending_root_reset().unwrap(),
        Some(offer.folder_id)
    );
}

#[tokio::test]
async fn worker_free_access_recovery_lists_repairs_and_removes_across_reopen() {
    let a = Device::new("Mac", 43386);
    let b = Device::new("Docker", 43387);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, _, _) = share(&a, &b, &mut first, &mut second);
    let recovery = FolderSyncAccessRecovery::new(first, Arc::clone(&a.installation)).unwrap();
    assert_eq!(
        recovery.summaries().await.unwrap()[0].phase,
        SharingPhase::Ready
    );

    let replacement = a.root.join("repaired");
    std::fs::create_dir(&replacement).unwrap();
    recovery
        .repair_root(offer.offer_id, &replacement)
        .await
        .unwrap();
    recovery.remove(offer.offer_id).await.unwrap();
    recovery.remove(offer.offer_id).await.unwrap();
    drop(recovery);

    let mut reopened = a.reopen();
    assert_eq!(
        reopened.summaries().unwrap()[0].phase,
        SharingPhase::Removed
    );
    assert!(reopened.summaries().unwrap()[0].remote_removal_pending);
    assert!(matches!(
        reopened.outbound_records().unwrap()[0].record,
        FolderShareRecord::Removal(_)
    ));
    assert!(reopened.desired_settings().unwrap().folders.is_empty());
    assert!(replacement.is_dir());
}

fn refresh_transport(device: &Device, expected: &TransportBinding, candidate: &TransportBinding) {
    let config = device.engine.config().unwrap();
    let grant = config.trusted_peers.get(&expected.peer_id).unwrap();
    assert!(
        device
            .engine
            .refresh_trusted_peer_address(grant, expected, &candidate.address)
            .unwrap()
    );
}

fn peer_grant(device: &Device, peer: DeviceId) -> covalent_protocol::PeerGrant {
    device.engine.config().unwrap().trusted_peers[&peer].clone()
}

#[test]
fn address_refresh_is_revision_bound_and_recovers_without_any_folder() {
    let a = Device::new("Mac", 43401);
    let b = Device::new("Docker", 43402);
    pair(&a, &b);
    let mut journal = a.journal();
    let expected = b.transport();
    let mut candidate = expected.clone();
    candidate.address = "127.0.0.1:53402".into();
    let candidate_engine = SyncEngineBinding::new(
        b.engine.device_id(),
        b.installation.device_id().as_str(),
        "127.0.0.1:53403",
    )
    .unwrap();
    let prepared = journal
        .prepare_peer_address_refresh(
            b.engine.device_id(),
            &peer_grant(&a, b.engine.device_id()),
            &expected,
            &candidate,
            &candidate_engine,
        )
        .unwrap();

    let mut other = a.reopen();
    assert_eq!(
        other.begin_peer_address_refresh(&prepared).unwrap_err(),
        SharingError::InvalidState
    );
    assert!(journal.begin_peer_address_refresh(&prepared).unwrap());
    assert!(matches!(
        journal.desired_settings(),
        Err(SharingError::PersistenceUncertain)
    ));
    drop(journal);

    let mut reopened = a.reopen();
    assert_eq!(
        reopened.pending_peer_address_refresh().unwrap(),
        Some((b.engine.device_id(), false))
    );
    refresh_transport(&a, &expected, &candidate);
    assert_eq!(
        reopened.pending_peer_address_refresh().unwrap(),
        Some((b.engine.device_id(), true))
    );
    reopened
        .complete_peer_address_refresh(b.engine.device_id())
        .unwrap();
    assert!(reopened.desired_settings().unwrap().folders.is_empty());
    assert_eq!(
        reopened
            .snapshot
            .current_peer_routes
            .get(&b.engine.device_id()),
        Some(&candidate_engine)
    );
    let revision = reopened.revision();
    let retry = reopened
        .prepare_peer_address_refresh(
            b.engine.device_id(),
            &peer_grant(&a, b.engine.device_id()),
            &expected,
            &candidate,
            &candidate_engine,
        )
        .unwrap();
    assert!(retry.is_already_complete());
    assert!(!reopened.begin_peer_address_refresh(&retry).unwrap());
    assert_eq!(reopened.revision(), revision);
}

#[test]
fn refreshed_routes_preserve_signed_share_history_and_drive_settings() {
    let a = Device::new("Mac", 43411);
    let b = Device::new("Docker", 43412);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, acceptance, _) = share(&a, &b, &mut first, &mut second);
    let signed_offer = serde_json::to_vec(&first.snapshot.shares[0].offer).unwrap();
    let signed_acceptance = serde_json::to_vec(&acceptance).unwrap();
    let expected = b.transport();
    let mut candidate = expected.clone();
    candidate.address = "127.0.0.1:53412".into();
    let candidate_engine = SyncEngineBinding::new(
        b.engine.device_id(),
        b.installation.device_id().as_str(),
        "127.0.0.1:53413",
    )
    .unwrap();
    let prepared = first
        .prepare_peer_address_refresh(
            b.engine.device_id(),
            &peer_grant(&a, b.engine.device_id()),
            &expected,
            &candidate,
            &candidate_engine,
        )
        .unwrap();
    first.begin_peer_address_refresh(&prepared).unwrap();
    refresh_transport(&a, &expected, &candidate);
    first
        .complete_peer_address_refresh(b.engine.device_id())
        .unwrap();

    assert_eq!(
        serde_json::to_vec(&first.snapshot.shares[0].offer).unwrap(),
        signed_offer
    );
    assert_eq!(
        serde_json::to_vec(first.snapshot.shares[0].acceptance.as_ref().unwrap()).unwrap(),
        signed_acceptance
    );
    assert_eq!(first.snapshot.shares[0].offer.offer_id, offer.offer_id);
    let settings = first.desired_settings().unwrap();
    assert_eq!(settings.peers[0].address().to_string(), "127.0.0.1:53413");
}

#[test]
fn cold_core_new_address_recovery_preserves_and_retargets_removal_outbox() {
    let a = Device::new("Mac", 43415);
    let b = Device::new("Docker", 43416);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let (offer, _, _) = share(&a, &b, &mut first, &mut second);
    first.remove(offer.offer_id).unwrap();
    assert_eq!(first.snapshot.pending_remote_removals.len(), 1);
    let expected = b.transport();
    let mut candidate = expected.clone();
    candidate.address = "127.0.0.1:53416".into();
    let candidate_engine = SyncEngineBinding::new(
        b.engine.device_id(),
        b.installation.device_id().as_str(),
        "127.0.0.1:53417",
    )
    .unwrap();
    let prepared = first
        .prepare_peer_address_refresh(
            b.engine.device_id(),
            &peer_grant(&a, b.engine.device_id()),
            &expected,
            &candidate,
            &candidate_engine,
        )
        .unwrap();
    first.begin_peer_address_refresh(&prepared).unwrap();
    refresh_transport(&a, &expected, &candidate);
    drop(first);

    let mut reopened = a.reopen();
    assert_eq!(
        reopened.summaries().unwrap()[0].phase,
        SharingPhase::Removed
    );
    assert_eq!(reopened.snapshot.pending_remote_removals.len(), 1);
    assert_eq!(
        reopened.pending_peer_address_refresh().unwrap(),
        Some((b.engine.device_id(), true))
    );
    reopened
        .complete_peer_address_refresh(b.engine.device_id())
        .unwrap();
    let delivery = reopened.outbound_records().unwrap();
    assert_eq!(delivery.len(), 1);
    assert_eq!(delivery[0].peer_transport, candidate);
    assert!(matches!(delivery[0].record, FolderShareRecord::Removal(_)));
}

#[test]
fn cold_local_route_change_preserves_history_and_updates_listener() {
    let a = Device::new("Mac", 43421);
    let b = Device::new("Docker", 43422);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    share(&a, &b, &mut first, &mut second);
    let historical = first.snapshot.shares[0].offer.clone();
    drop(first);

    let listener: SocketAddr = "0.0.0.0:53421".parse().unwrap();
    let advertised: SocketAddr = "127.0.0.2:53421".parse().unwrap();
    let reopened = FolderSharingJournal::open_at_route(
        Arc::clone(&a.engine),
        Arc::clone(&a.installation),
        Arc::clone(&a.protector),
        listener,
        advertised,
    )
    .unwrap();
    assert_eq!(reopened.snapshot.shares[0].offer, historical);
    assert_eq!(reopened.listener(), listener);
    assert_eq!(reopened.binding().direct_address, advertised.to_string());
}

#[test]
fn signed_probe_can_refresh_engine_listener_without_changing_control_address() {
    let a = Device::new("Mac", 43425);
    let b = Device::new("Docker", 43426);
    pair(&a, &b);
    let mut journal = a.journal();
    let transport = b.transport();
    let candidate_engine = SyncEngineBinding::new(
        b.engine.device_id(),
        b.installation.device_id().as_str(),
        "127.0.0.1:53427",
    )
    .unwrap();
    let grant = peer_grant(&a, b.engine.device_id());
    let prepared = journal
        .prepare_peer_address_refresh(
            b.engine.device_id(),
            &grant,
            &transport,
            &transport,
            &candidate_engine,
        )
        .unwrap();
    assert!(journal.begin_peer_address_refresh(&prepared).unwrap());
    assert!(
        !a.engine
            .refresh_trusted_peer_address(&grant, &transport, &transport.address)
            .unwrap()
    );
    journal
        .complete_peer_address_refresh(b.engine.device_id())
        .unwrap();
    assert_eq!(
        journal
            .snapshot
            .current_peer_routes
            .get(&b.engine.device_id()),
        Some(&candidate_engine)
    );
}

#[test]
fn invitation_renewal_can_carry_the_authenticated_current_route() {
    let a = Device::new("Mac", 43431);
    let b = Device::new("Docker", 43432);
    pair(&a, &b);
    let mut first = a.journal();
    let mut second = b.journal();
    let old = first
        .offer(
            b.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &a.files(),
            2_000,
        )
        .unwrap();
    second.receive_offer(old.clone(), 2_001).unwrap();
    drop(first);
    let listener: SocketAddr = "0.0.0.0:53431".parse().unwrap();
    let advertised: SocketAddr = "127.0.0.2:53431".parse().unwrap();
    let mut first = FolderSharingJournal::open_at_route(
        Arc::clone(&a.engine),
        Arc::clone(&a.installation),
        Arc::clone(&a.protector),
        listener,
        advertised,
    )
    .unwrap();

    let expected_transport = a.transport();
    let mut candidate_transport = expected_transport.clone();
    candidate_transport.address = "127.0.0.2:43431".into();
    let prepared = second
        .prepare_peer_address_refresh(
            a.engine.device_id(),
            &peer_grant(&b, a.engine.device_id()),
            &expected_transport,
            &candidate_transport,
            first.binding(),
        )
        .unwrap();
    second.begin_peer_address_refresh(&prepared).unwrap();
    refresh_transport(&b, &expected_transport, &candidate_transport);
    second
        .complete_peer_address_refresh(a.engine.device_id())
        .unwrap();

    let renewed = first
        .renew_offer(old.offer_id, old.expires_at_unix_ms)
        .unwrap();
    assert_ne!(
        renewed.source_engine.direct_address,
        old.source_engine.direct_address
    );
    second
        .receive_offer(renewed.clone(), renewed.issued_at_unix_ms)
        .unwrap();
    assert_eq!(second.snapshot.shares[0].offer, renewed);
}
