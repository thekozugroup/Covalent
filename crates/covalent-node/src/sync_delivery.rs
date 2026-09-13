//! Bounded, restartable delivery of already durable folder-consent records.

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    fn batch(
        &mut self,
        deliveries: Vec<FolderShareDelivery>,
        now: u64,
        busy_peers: &BTreeSet<DeviceId>,
        limit: usize,
    ) -> Vec<Pending> {
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
            if !busy_peers.contains(&delivery.peer_transport.peer_id)
                && !self.acknowledged.contains(&key)
            {
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
        for offset in 0..MAX_PEERS_PER_BATCH.min(limit).min(peers.len()) {
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
    let mut requests = JoinSet::new();
    let mut request_peers = HashMap::new();
    let mut busy_peers = BTreeSet::new();
    loop {
        if *shutdown.borrow() {
            abort_and_reap(&mut requests).await;
            return;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    abort_and_reap(&mut requests).await;
                    return;
                }
            }
            result = requests.join_next_with_id(), if !requests.is_empty() => {
                let Some(result) = result else { continue; };
                let (id, key) = match result {
                    Ok((id, key)) => (id, key),
                    Err(error) => (error.id(), None),
                };
                if let Some(peer) = request_peers.remove(&id) {
                    busy_peers.remove(&peer);
                }
                if let Some(key) = key {
                    cursor.acknowledged.insert(key);
                }
            }
            _ = interval.tick() => {
                let capacity = MAX_PEERS_PER_BATCH.saturating_sub(requests.len());
                if capacity == 0 {
                    continue;
                }
                let records = tokio::select! {
                    changed = shutdown.changed() => {
                        let _ = changed;
                        abort_and_reap(&mut requests).await;
                        return;
                    }
                    records = service.outbound_records() => records,
                };
                let Ok(records) = records else { continue; };
                let batch = cursor.batch(
                    records.into_value(),
                    crate::now_unix_ms(),
                    &busy_peers,
                    capacity,
                );
                for pending in batch {
                    let peer = pending.delivery.peer_transport.peer_id;
                    let engine = Arc::clone(&engine);
                    let service = Arc::clone(&service);
                    let handle = requests.spawn(async move { deliver(engine, service, pending).await });
                    request_peers.insert(handle.id(), peer);
                    busy_peers.insert(peer);
                }
            }
        }
    }
}

async fn abort_and_reap<T: 'static>(requests: &mut JoinSet<T>) {
    requests.abort_all();
    while requests.join_next().await.is_some() {}
}

async fn deliver(
    engine: Arc<Engine>,
    service: Arc<FolderSyncService>,
    pending: Pending,
) -> Option<[u8; 32]> {
    if let FolderShareRecord::SettingsRequest(request) = &pending.delivery.record {
        let source_id = pending.delivery.peer_transport.peer_id;
        let target_id = engine.device_id();
        let response = send_folder_control(
            engine,
            pending.delivery.peer_transport,
            FolderControlOperation::RequestLinkSettings(request.clone()),
        )
        .await
        .ok()?;
        let FolderControlPayload::LinkSettings(commit) = response else {
            return None;
        };
        if commit.source_id != source_id
            || commit.target_id != target_id
            || commit.folder_id != request.folder_id
        {
            return None;
        }
        service.receive_link_settings_commit(&commit).await.ok()?;
        return Some(pending.key);
    }
    if let FolderShareRecord::RunRequest(request) = &pending.delivery.record {
        let source_id = pending.delivery.peer_transport.peer_id;
        let target_id = engine.device_id();
        let response = send_folder_control(
            engine,
            pending.delivery.peer_transport,
            FolderControlOperation::RequestLinkRun(request.clone()),
        )
        .await
        .ok()?;
        let FolderControlPayload::LinkRun(commit) = response else {
            return None;
        };
        if commit.source_id != source_id
            || commit.target_id != target_id
            || commit.folder_id != request.folder_id
        {
            return None;
        }
        service.receive_link_run_commit(&commit).await.ok()?;
        return Some(pending.key);
    }
    let (operation, acceptance_id, removal_ack) = match pending.delivery.record {
        FolderShareRecord::SettingsCommit(commit) => (
            FolderControlOperation::CommitLinkSettings(commit),
            None,
            None,
        ),
        FolderShareRecord::SettingsRequest(_) | FolderShareRecord::RunRequest(_) => return None,
        FolderShareRecord::RunCommit(commit) => {
            (FolderControlOperation::CommitLinkRun(commit), None, None)
        }
        FolderShareRecord::RunReport(report) => {
            (FolderControlOperation::ReportLinkRun(report), None, None)
        }
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
    use std::sync::atomic::{AtomicUsize, Ordering};
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
        let first = cursor.batch(deliveries.clone(), 1, &BTreeSet::new(), usize::MAX);
        let second = cursor.batch(deliveries.clone(), 2, &BTreeSet::new(), 4);
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
        assert!(
            cursor
                .batch(deliveries.clone(), 3, &BTreeSet::new(), 4)
                .is_empty()
        );
        assert_eq!(
            DeliveryCursor::default()
                .batch(deliveries, 4, &BTreeSet::new(), 4)
                .len(),
            4
        );
        assert!(cursor.batch(Vec::new(), 5, &BTreeSet::new(), 4).is_empty());
        assert!(cursor.acknowledged.is_empty());
    }

    #[test]
    fn busy_peer_does_not_block_later_records_for_a_ready_peer() {
        let slow = DeviceId::from_uuid(Uuid::from_u128(1));
        let ready = DeviceId::from_uuid(Uuid::from_u128(2));
        let records = vec![
            delivery(slow, Uuid::from_u128(1)),
            delivery(ready, Uuid::from_u128(2)),
            delivery(slow, Uuid::from_u128(3)),
            delivery(ready, Uuid::from_u128(4)),
        ];
        let mut cursor = DeliveryCursor::default();
        let first = cursor.batch(records.clone(), 1, &BTreeSet::new(), 4);
        assert_eq!(first.len(), 2);
        assert_eq!(
            first
                .iter()
                .map(|pending| pending.delivery.peer_transport.peer_id)
                .collect::<BTreeSet<_>>()
                .len(),
            first.len(),
            "only one request per peer may be in flight"
        );
        let ready_first = first
            .iter()
            .find(|pending| pending.delivery.peer_transport.peer_id == ready)
            .unwrap();
        cursor.acknowledged.insert(ready_first.key);
        let next = cursor.batch(records, 2, &BTreeSet::from([slow]), 3);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].delivery.peer_transport.peer_id, ready);
    }

    #[tokio::test]
    async fn shutdown_aborts_and_reaps_in_flight_deliveries() {
        struct Dropped(Arc<AtomicUsize>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicUsize::new(0));
        let mut requests = JoinSet::new();
        for _ in 0..MAX_PEERS_PER_BATCH {
            let dropped = Arc::clone(&dropped);
            requests.spawn(async move {
                let _guard = Dropped(dropped);
                std::future::pending::<()>().await;
            });
        }
        tokio::task::yield_now().await;
        abort_and_reap(&mut requests).await;
        assert!(requests.is_empty());
        assert_eq!(dropped.load(Ordering::SeqCst), MAX_PEERS_PER_BATCH);
    }
}
