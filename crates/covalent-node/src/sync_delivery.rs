//! Bounded, restartable delivery of already durable folder-consent records.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use covalent_core::Engine;
use covalent_protocol::DeviceId;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::sync_control::{FolderControlOperation, FolderControlPayload, send_folder_control};
use crate::sync_engine::{FolderShareDelivery, FolderShareRecord, FolderSyncService};

const CADENCE: Duration = Duration::from_secs(5);
const MAX_PEERS_PER_BATCH: usize = 4;

struct Pending {
    key: [u8; 32],
    delivery: FolderShareDelivery,
}

#[derive(Default)]
struct DeliveryCursor {
    acknowledged: BTreeSet<[u8; 32]>,
    last_peer: Option<DeviceId>,
}

impl DeliveryCursor {
    fn batch(&mut self, deliveries: Vec<FolderShareDelivery>, now: u64) -> Vec<Pending> {
        let mut retained = BTreeSet::new();
        let mut per_peer = BTreeMap::new();
        for delivery in deliveries {
            // Signatures on accepted/committed decisions remain replayable
            // after the original offer window. A never-accepted expired offer
            // should be renewed through the user flow, not dialed indefinitely.
            if matches!(&delivery.record, FolderShareRecord::Offer(offer) if now >= offer.expires_at_unix_ms)
            {
                continue;
            }
            let Ok(bytes) = serde_json::to_vec(&(&delivery.peer_transport, &delivery.record))
            else {
                continue;
            };
            let key = *blake3::hash(&bytes).as_bytes();
            retained.insert(key);
            if !self.acknowledged.contains(&key) {
                per_peer
                    .entry(delivery.peer_transport.peer_id)
                    .or_insert(Pending { key, delivery });
            }
        }
        self.acknowledged.retain(|key| retained.contains(key));
        let peers = per_peer.keys().copied().collect::<Vec<_>>();
        if peers.is_empty() {
            return Vec::new();
        }
        let start = self.last_peer.map_or(0, |last| {
            peers.partition_point(|id| *id <= last) % peers.len()
        });
        let mut selected = Vec::new();
        for offset in 0..MAX_PEERS_PER_BATCH.min(peers.len()) {
            let peer = peers[(start + offset) % peers.len()];
            self.last_peer = Some(peer);
            if let Some(pending) = per_peer.remove(&peer) {
                selected.push(pending);
            }
        }
        selected
    }
}

pub(crate) async fn run(
    engine: Arc<Engine>,
    service: Arc<FolderSyncService>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(CADENCE);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut cursor = DeliveryCursor::default();
    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { return; }
            }
            _ = interval.tick() => {
                let records = tokio::select! {
                    changed = shutdown.changed() => { let _ = changed; return; }
                    records = service.outbound_records() => records,
                };
                let Ok(records) = records else { continue; };
                let batch = cursor.batch(records.into_value(), crate::now_unix_ms());
                let mut requests = JoinSet::new();
                for pending in batch {
                    let engine = Arc::clone(&engine);
                    let service = Arc::clone(&service);
                    requests.spawn(async move {
                        deliver(engine, service, pending).await
                    });
                }
                while !requests.is_empty() {
                    tokio::select! {
                        changed = shutdown.changed() => {
                            let _ = changed;
                            requests.abort_all();
                            while requests.join_next().await.is_some() {}
                            return;
                        }
                        result = requests.join_next() => {
                            if let Some(Ok(Some(key))) = result {
                                cursor.acknowledged.insert(key);
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn deliver(
    engine: Arc<Engine>,
    service: Arc<FolderSyncService>,
    pending: Pending,
) -> Option<[u8; 32]> {
    let (operation, acceptance_id, removal_ack) = match pending.delivery.record {
        FolderShareRecord::Offer(offer) => (FolderControlOperation::SendOffer(offer), None, None),
        FolderShareRecord::Acceptance {
            offer_id,
            acceptance,
        } => (
            FolderControlOperation::SendAcceptance {
                offer_id,
                acceptance,
            },
            Some(offer_id),
            None,
        ),
        FolderShareRecord::Commit { offer_id, commit } => (
            FolderControlOperation::SendCommit { offer_id, commit },
            None,
            None,
        ),
        FolderShareRecord::Removal(notice) => {
            let current = *notice.offer_ids.last()?;
            let peer = notice.target_id;
            (
                FolderControlOperation::SendRemoval(notice),
                None,
                Some((peer, current)),
            )
        }
    };
    let response = send_folder_control(engine, pending.delivery.peer_transport, operation)
        .await
        .ok()?;
    match (acceptance_id, removal_ack, response) {
        (None, Some((peer, offer_id)), FolderControlPayload::Ack) => {
            service.acknowledge_removal(peer, offer_id).await.ok()?;
            Some(pending.key)
        }
        (None, None, FolderControlPayload::Ack) => Some(pending.key),
        (Some(offer_id), None, FolderControlPayload::Commit(commit)) => {
            service.receive_commit(offer_id, commit).await.ok()?;
            Some(pending.key)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covalent_protocol::{FolderShareCommit, TransportBinding};
    use uuid::Uuid;

    fn delivery(peer: DeviceId, offer_id: Uuid) -> FolderShareDelivery {
        FolderShareDelivery {
            peer_transport: TransportBinding {
                peer_id: peer,
                display_name: "Peer".into(),
                address: "127.0.0.1:8788".into(),
                certificate_der: "public-fixture".into(),
                certificate_fingerprint: "00".repeat(32),
            },
            record: FolderShareRecord::Commit {
                offer_id,
                commit: FolderShareCommit {
                    schema_version: 1,
                    source_device_id: DeviceId::from_uuid(Uuid::from_u128(999)),
                    offer_digest: "11".repeat(32),
                    acceptance_digest: "22".repeat(32),
                    signature: "fixture".into(),
                },
            },
        }
    }

    #[test]
    fn offline_peers_do_not_starve_later_peers_and_cold_restart_replays_durable_records() {
        let deliveries = (1..=8)
            .map(|i| delivery(DeviceId::from_uuid(Uuid::from_u128(i)), Uuid::new_v4()))
            .collect::<Vec<_>>();
        let mut cursor = DeliveryCursor::default();
        let first = cursor.batch(deliveries.clone(), 1);
        let second = cursor.batch(deliveries.clone(), 2);
        assert_eq!(first.len(), 4);
        assert_eq!(second.len(), 4);
        let first_peers = first
            .iter()
            .map(|item| item.delivery.peer_transport.peer_id)
            .collect::<BTreeSet<_>>();
        assert!(
            second
                .iter()
                .all(|item| !first_peers.contains(&item.delivery.peer_transport.peer_id))
        );
        cursor
            .acknowledged
            .extend(first.iter().chain(second.iter()).map(|item| item.key));
        assert!(cursor.batch(deliveries.clone(), 3).is_empty());
        assert_eq!(DeliveryCursor::default().batch(deliveries, 4).len(), 4);
        assert!(cursor.batch(Vec::new(), 5).is_empty());
        assert!(cursor.acknowledged.is_empty());
    }
}
