use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use covalent_protocol::{BackupId, ChunkReference, DeviceId, ReplicaIntent};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::engine::JobControl;
use crate::{BackupKey, ChunkStore, CoreError, EncryptedChunk, RecoveryCapsule};

const MAXIMUM_FETCH_BATCH_BYTES: u64 = 2 * 1_024 * 1_024;
const MAXIMUM_VERIFICATION_BATCH_BYTES: u64 = 2 * 1_024 * 1_024;
// The network transport sends this batch as one authenticated binary payload.
// Sixteen MiB amortizes the provider journal/fsync boundary while keeping the
// aggregate client/server buffers comfortably below the streaming RSS budget.
const MAXIMUM_WRITE_BATCH_RECORDS: usize = 64;
const MAXIMUM_WRITE_BATCH_BYTES: usize = 16 * 1_024 * 1_024;
const PIPELINE_LEASE_SEGMENT_BYTES: u64 = 256 * 1_024 * 1_024;
const PIPELINE_QUEUE_SEGMENTS: usize = 8;

/// Coarse provider reachability and integrity state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealth {
    /// Reachable and accepting requests.
    Online,
    /// Explicitly unavailable.
    Offline,
    /// Returned at least one corrupt object in this operation.
    Corrupt,
}

/// One provider/object failure retained for visible degraded state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderFailure {
    /// Provider that failed.
    pub provider_id: DeviceId,
    /// Affected locator, when known.
    pub locator: Option<String>,
    /// Stable non-secret failure category.
    pub reason: String,
}

/// Explicit replication outcome. Missing acknowledgements are never backfilled elsewhere.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplicationReport {
    /// Durable object acknowledgements per explicitly selected provider.
    pub acknowledgements: BTreeMap<DeviceId, BTreeSet<String>>,
    /// Providers that durably committed the signed encrypted recovery capsule.
    pub recovery_catalog_acknowledgements: BTreeSet<DeviceId>,
    /// Current state for every selected provider.
    pub provider_health: BTreeMap<DeviceId, ProviderHealth>,
    /// Visible per-object errors.
    pub failures: Vec<ProviderFailure>,
}

impl ReplicationReport {
    /// True when every selected provider acknowledged every object.
    #[must_use]
    pub fn is_complete(&self, required_objects: usize) -> bool {
        self.failures.is_empty()
            && self
                .provider_health
                .values()
                .all(|health| *health == ProviderHealth::Online)
            && self
                .acknowledgements
                .values()
                .all(|locators| locators.len() == required_objects)
            && self.recovery_catalog_acknowledgements.len() == self.acknowledgements.len()
    }
}

/// Authorized encrypted storage provider boundary.
pub trait ChunkProvider: Send + Sync {
    /// Stable paired device identity.
    fn device_id(&self) -> DeviceId;
    /// Current transport reachability.
    fn health(&self) -> ProviderHealth;
    /// Reserves one bounded lease for the exact backup replication batch.
    fn begin_backup_write(
        &self,
        _backup_id: BackupId,
        _maximum_new_bytes: u64,
        _maximum_new_objects: u64,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    /// Releases the exact backup-scoped lease after all write attempts finish.
    /// Network providers use this boundary to durably cancel unused reservation
    /// and clear their restart journal; local providers need no action.
    fn finish_backup_write(&self, _backup_id: BackupId) -> Result<(), CoreError> {
        Ok(())
    }
    /// Durably stores one opaque encrypted record.
    fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError>;
    /// Durably stores one record under an exact backup-scoped provider lease.
    fn put_scoped(
        &self,
        _backup_id: BackupId,
        locator: &str,
        record: &[u8],
    ) -> Result<(), CoreError> {
        self.put(locator, record)
    }
    /// Durably stores an exact bounded backup-scoped batch while observing
    /// cancellation between records. Network providers override this with one
    /// authenticated lease-bound transport operation.
    fn put_many_scoped_controlled(
        &self,
        backup_id: BackupId,
        records: &[(String, Vec<u8>)],
        control: &JobControl,
    ) -> Result<(), CoreError> {
        for (locator, record) in records {
            control.check()?;
            self.put_scoped(backup_id, locator, record)?;
        }
        control.check()
    }
    /// Fetches one bounded opaque encrypted record.
    fn get(&self, locator: &str) -> Result<Vec<u8>, CoreError>;
    /// Fetches one opaque record bound to an exact authorized backup scope.
    fn get_scoped(&self, _backup_id: BackupId, locator: &str) -> Result<Vec<u8>, CoreError> {
        self.get(locator)
    }
    /// Fetches an exact ordered batch while observing pause/cancel between
    /// bounded provider operations. Network providers should override this to
    /// cancel the in-flight transport rather than waiting for its I/O timeout.
    fn get_many_controlled(
        &self,
        backup_id: BackupId,
        locators: &[String],
        control: &JobControl,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let mut records = Vec::with_capacity(locators.len());
        for locator in locators {
            control.check()?;
            records.push(self.get_scoped(backup_id, locator)?);
        }
        control.check()?;
        Ok(records)
    }
    /// Returns whether this provider currently advertises the object.
    fn contains(&self, locator: &str) -> Result<bool, CoreError>;
    /// Returns scoped availability without exposing objects from another backup.
    fn contains_scoped(&self, _backup_id: BackupId, locator: &str) -> Result<bool, CoreError> {
        self.contains(locator)
    }
    /// Durably stores one owner-signed encrypted recovery catalog.
    fn put_recovery_capsule(&self, _capsule: &RecoveryCapsule) -> Result<(), CoreError> {
        Err(CoreError::InvalidState(
            "provider does not support recovery catalogs".to_owned(),
        ))
    }
    /// Durably stores one recovery capsule under its backup-scoped lease.
    fn put_recovery_capsule_scoped(
        &self,
        backup_id: BackupId,
        capsule: &RecoveryCapsule,
    ) -> Result<(), CoreError> {
        if backup_id != capsule.backup_id {
            return Err(CoreError::AuthenticationFailed);
        }
        self.put_recovery_capsule(capsule)
    }
    /// Lists bounded opaque catalogs for authentication by a recovered owner.
    fn list_recovery_capsules(&self) -> Result<Vec<RecoveryCapsule>, CoreError> {
        Err(CoreError::InvalidState(
            "provider does not support recovery catalogs".to_owned(),
        ))
    }
}

/// Adapter exposing a local `ChunkStore` as one explicitly identified provider.
#[derive(Clone, Debug)]
pub struct StoreProvider {
    device_id: DeviceId,
    store: ChunkStore,
}

impl StoreProvider {
    /// Creates the adapter. Registration remains an explicit caller action.
    #[must_use]
    pub const fn new(device_id: DeviceId, store: ChunkStore) -> Self {
        Self { device_id, store }
    }
}

impl ChunkProvider for StoreProvider {
    fn device_id(&self) -> DeviceId {
        self.device_id
    }

    fn health(&self) -> ProviderHealth {
        ProviderHealth::Online
    }

    fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError> {
        self.store.put_provider_record(locator, record).map(|_| ())
    }

    fn get(&self, locator: &str) -> Result<Vec<u8>, CoreError> {
        self.store.get_provider_record(locator)
    }

    fn contains(&self, locator: &str) -> Result<bool, CoreError> {
        self.store.contains(locator)
    }

    fn put_recovery_capsule(&self, capsule: &RecoveryCapsule) -> Result<(), CoreError> {
        self.store.put_recovery_capsule(capsule).map(|_| ())
    }

    fn list_recovery_capsules(&self) -> Result<Vec<RecoveryCapsule>, CoreError> {
        self.store.list_recovery_capsules()
    }
}

/// Bounded scheduler for explicit replication and multi-source verified reads.
#[derive(Clone)]
pub struct ReplicationScheduler {
    providers: Arc<BTreeMap<DeviceId, Arc<dyn ChunkProvider>>>,
    maximum_parallelism: usize,
}

pub(crate) struct ReplicationPipeline {
    sender: Option<SyncSender<Vec<String>>>,
    worker: Option<JoinHandle<Result<ReplicationReport, CoreError>>>,
}

impl ReplicationPipeline {
    pub(crate) fn submit(&self, locators: &[String]) -> Result<(), CoreError> {
        if locators.is_empty() {
            return Ok(());
        }
        self.sender
            .as_ref()
            .ok_or_else(|| CoreError::InvalidState("replication pipeline is closed".to_owned()))?
            .send(locators.to_vec())
            .map_err(|_| CoreError::Synchronization)
    }

    pub(crate) fn finish(mut self) -> Result<ReplicationReport, CoreError> {
        self.sender.take();
        self.worker
            .take()
            .ok_or(CoreError::Synchronization)?
            .join()
            .map_err(|_| CoreError::Synchronization)?
    }
}

impl Drop for ReplicationPipeline {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl ReplicationScheduler {
    /// Registers an exact authorized provider set.
    pub fn new(
        providers: impl IntoIterator<Item = Arc<dyn ChunkProvider>>,
        maximum_parallelism: usize,
    ) -> Result<Self, CoreError> {
        if !(1..=32).contains(&maximum_parallelism) {
            return Err(CoreError::ResourceLimit("replication parallelism"));
        }
        let mut indexed = BTreeMap::new();
        for provider in providers {
            if indexed.insert(provider.device_id(), provider).is_some() || indexed.len() > 128 {
                return Err(CoreError::InvalidState(
                    "duplicate or excessive provider registration".to_owned(),
                ));
            }
        }
        Ok(Self {
            providers: Arc::new(indexed),
            maximum_parallelism,
        })
    }

    pub(crate) fn replicate_recovery_capsule(
        &self,
        intent: &ReplicaIntent,
        capsule: &RecoveryCapsule,
        report: &mut ReplicationReport,
    ) {
        for provider_id in &intent.selected_providers {
            let Some(provider) = self.providers.get(provider_id) else {
                report.failures.push(ProviderFailure {
                    provider_id: *provider_id,
                    locator: None,
                    reason: "recovery_catalog_provider_unavailable".to_owned(),
                });
                continue;
            };
            if provider.health() != ProviderHealth::Online {
                report.failures.push(ProviderFailure {
                    provider_id: *provider_id,
                    locator: None,
                    reason: "recovery_catalog_provider_offline".to_owned(),
                });
                continue;
            }
            match provider.put_recovery_capsule_scoped(capsule.backup_id, capsule) {
                Ok(()) => {
                    report
                        .recovery_catalog_acknowledgements
                        .insert(*provider_id);
                }
                Err(error) => report.failures.push(ProviderFailure {
                    provider_id: *provider_id,
                    locator: None,
                    reason: format!("recovery_catalog_{}", error_category(&error)),
                }),
            }
        }
        report.failures.sort_by(|left, right| {
            (left.provider_id, &left.locator, &left.reason).cmp(&(
                right.provider_id,
                &right.locator,
                &right.reason,
            ))
        });
    }

    pub(crate) fn recovery_capsules(&self) -> Result<Vec<(DeviceId, RecoveryCapsule)>, CoreError> {
        let mut capsules = Vec::new();
        for (provider_id, provider) in self.providers.iter() {
            if provider.health() != ProviderHealth::Online {
                continue;
            }
            for capsule in provider.list_recovery_capsules()? {
                capsules.push((*provider_id, capsule));
                if capsules.len() > 1_000_000 {
                    return Err(CoreError::ResourceLimit("recovery catalog listing"));
                }
            }
        }
        capsules.sort_by(|left, right| {
            (left.1.backup_id, &left.1.snapshot_id, left.0).cmp(&(
                right.1.backup_id,
                &right.1.snapshot_id,
                right.0,
            ))
        });
        Ok(capsules)
    }

    /// No-provider scheduler for local-only backups.
    #[must_use]
    pub fn local_only() -> Self {
        Self {
            providers: Arc::new(BTreeMap::new()),
            maximum_parallelism: 1,
        }
    }

    /// Exact authorized providers currently registered with this scheduler.
    #[must_use]
    pub fn provider_ids(&self) -> BTreeSet<DeviceId> {
        self.providers.keys().copied().collect()
    }

    /// Returns a scheduler with one revoked provider removed immediately.
    pub(crate) fn without_provider(&self, provider_id: DeviceId) -> Self {
        let mut providers = (*self.providers).clone();
        providers.remove(&provider_id);
        Self {
            providers: Arc::new(providers),
            maximum_parallelism: self.maximum_parallelism,
        }
    }

    pub(crate) fn provider_health(&self, provider_id: DeviceId) -> Option<ProviderHealth> {
        self.providers
            .get(&provider_id)
            .map(|provider| provider.health())
    }

    pub(crate) const fn maximum_parallelism(&self) -> usize {
        self.maximum_parallelism
    }

    pub(crate) const fn maximum_fetch_batch(&self) -> usize {
        let batch = self.maximum_parallelism.saturating_mul(4);
        if batch < 32 { batch } else { 32 }
    }

    pub(crate) const fn maximum_fetch_batch_bytes(&self) -> u64 {
        MAXIMUM_FETCH_BATCH_BYTES
    }

    pub(crate) fn start_pipeline(
        &self,
        source: ChunkStore,
        intent: ReplicaIntent,
        control: JobControl,
        backup_id: BackupId,
    ) -> ReplicationPipeline {
        let scheduler = self.clone();
        let (sender, receiver) = sync_channel(PIPELINE_QUEUE_SEGMENTS);
        let worker = std::thread::spawn(move || {
            scheduler.run_pipeline(source, intent, control, backup_id, receiver)
        });
        ReplicationPipeline {
            sender: Some(sender),
            worker: Some(worker),
        }
    }

    fn run_pipeline(
        &self,
        source: ChunkStore,
        intent: ReplicaIntent,
        control: JobControl,
        backup_id: BackupId,
        receiver: Receiver<Vec<String>>,
    ) -> Result<ReplicationReport, CoreError> {
        let mut report = ReplicationReport::default();
        let mut segment = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut segment_bytes = 0_u64;
        let mut saw_segment = false;
        for batch in receiver {
            control.check()?;
            for locator in batch {
                if !seen.insert(locator.clone()) {
                    continue;
                }
                let record_bytes = source.provider_record_length(&locator)?;
                if !segment.is_empty()
                    && segment_bytes.saturating_add(record_bytes) > PIPELINE_LEASE_SEGMENT_BYTES
                {
                    let part =
                        self.replicate_controlled(&source, &intent, &segment, &control, backup_id)?;
                    merge_replication_report(&mut report, part);
                    segment.clear();
                    segment_bytes = 0;
                    saw_segment = true;
                }
                if segment.insert(locator) {
                    segment_bytes = segment_bytes
                        .checked_add(record_bytes)
                        .ok_or(CoreError::ResourceLimit("replication pipeline segment"))?;
                }
            }
        }
        if !segment.is_empty() || !saw_segment {
            let part =
                self.replicate_controlled(&source, &intent, &segment, &control, backup_id)?;
            merge_replication_report(&mut report, part);
        }
        Ok(report)
    }

    /// Authenticates a bounded batch of provider copies across the configured
    /// worker pool. Every returned failure is tied to its exact peer and object.
    pub(crate) fn verify_provider_copies_parallel(
        &self,
        copies: &[(DeviceId, &ChunkReference)],
        backup_id: BackupId,
        key: &BackupKey,
    ) -> Result<Vec<ProviderFailure>, CoreError> {
        if copies.is_empty() {
            return Ok(Vec::new());
        }
        let mut grouped = BTreeMap::<DeviceId, Vec<&ChunkReference>>::new();
        for (provider_id, reference) in copies {
            grouped.entry(*provider_id).or_default().push(reference);
        }
        let mut jobs = Vec::new();
        let mut unavailable = Vec::new();
        for (provider_id, references) in grouped {
            let Some(provider) = self.providers.get(&provider_id) else {
                unavailable.extend(references.into_iter().map(|reference| ProviderFailure {
                    provider_id,
                    locator: Some(reference.opaque_locator.clone()),
                    reason: "provider_unavailable".to_owned(),
                }));
                continue;
            };
            let mut cursor = 0;
            while cursor < references.len() {
                let start = cursor;
                let mut batch_bytes = 0_u64;
                while cursor < references.len() && cursor - start < self.maximum_fetch_batch() {
                    let record_bytes =
                        u64::from(references[cursor].ciphertext_length).saturating_add(64);
                    if cursor > start
                        && batch_bytes.saturating_add(record_bytes)
                            > MAXIMUM_VERIFICATION_BATCH_BYTES
                    {
                        break;
                    }
                    batch_bytes = batch_bytes.saturating_add(record_bytes);
                    cursor += 1;
                }
                jobs.push((
                    provider_id,
                    Arc::clone(provider),
                    references[start..cursor].to_vec(),
                ));
            }
        }
        let next = AtomicUsize::new(0);
        let failures = Mutex::new(unavailable);
        // One large response at a time keeps base64/JSON copies below the
        // streaming RSS ceiling while still collapsing per-object round trips.
        let worker_count = usize::from(!jobs.is_empty());
        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                let next = &next;
                let failures = &failures;
                let jobs = &jobs;
                scope.spawn(move || {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some((provider_id, provider, batch)) = jobs.get(index) else {
                            break;
                        };
                        let locators = batch
                            .iter()
                            .map(|reference| reference.opaque_locator.clone())
                            .collect::<Vec<_>>();
                        match provider.get_many_controlled(backup_id, &locators, &JobControl::new())
                        {
                            Ok(records) if records.len() == batch.len() => {
                                for (reference, record) in batch.iter().zip(records) {
                                    if let Err(error) =
                                        verify_provider_record(reference, backup_id, key, &record)
                                        && let Ok(mut values) = failures.lock()
                                    {
                                        values.push(ProviderFailure {
                                            provider_id: *provider_id,
                                            locator: Some(reference.opaque_locator.clone()),
                                            reason: error_category(&error).to_owned(),
                                        });
                                    }
                                }
                            }
                            Ok(_) => {
                                if let Ok(mut values) = failures.lock() {
                                    values.extend(batch.iter().map(|reference| ProviderFailure {
                                        provider_id: *provider_id,
                                        locator: Some(reference.opaque_locator.clone()),
                                        reason: "authentication_failed".to_owned(),
                                    }));
                                }
                            }
                            Err(error) => {
                                let reason = error_category(&error).to_owned();
                                if let Ok(mut values) = failures.lock() {
                                    values.extend(batch.iter().map(|reference| ProviderFailure {
                                        provider_id: *provider_id,
                                        locator: Some(reference.opaque_locator.clone()),
                                        reason: reason.clone(),
                                    }));
                                }
                            }
                        }
                    }
                });
            }
        });
        let mut failures = failures
            .into_inner()
            .map_err(|_| CoreError::Synchronization)?;
        failures.sort_by(|left, right| {
            (left.provider_id, &left.locator).cmp(&(right.provider_id, &right.locator))
        });
        Ok(failures)
    }

    /// Sends records only to device IDs present in exact explicit intent.
    pub fn replicate(
        &self,
        source: &ChunkStore,
        intent: &ReplicaIntent,
        locators: &BTreeSet<String>,
    ) -> Result<ReplicationReport, CoreError> {
        self.replicate_controlled(
            source,
            intent,
            locators,
            &JobControl::new(),
            BackupId::default(),
        )
    }

    pub(crate) fn replicate_controlled(
        &self,
        source: &ChunkStore,
        intent: &ReplicaIntent,
        locators: &BTreeSet<String>,
        control: &JobControl,
        backup_id: BackupId,
    ) -> Result<ReplicationReport, CoreError> {
        control.check()?;
        let mut report = ReplicationReport::default();
        let mut online_providers = Vec::new();
        for provider_id in &intent.selected_providers {
            report
                .acknowledgements
                .insert(*provider_id, BTreeSet::new());
            let Some(provider) = self.providers.get(provider_id) else {
                report
                    .provider_health
                    .insert(*provider_id, ProviderHealth::Offline);
                report.failures.push(ProviderFailure {
                    provider_id: *provider_id,
                    locator: None,
                    reason: "provider_unavailable".to_owned(),
                });
                continue;
            };
            let health = provider.health();
            report.provider_health.insert(*provider_id, health);
            if health != ProviderHealth::Online {
                report.failures.push(ProviderFailure {
                    provider_id: *provider_id,
                    locator: None,
                    reason: "provider_offline".to_owned(),
                });
                continue;
            }
            online_providers.push((*provider_id, Arc::clone(provider)));
        }

        let locator_list: Vec<_> = locators.iter().cloned().collect();
        let mut maximum_new_bytes = 0_u64;
        for locator in &locator_list {
            maximum_new_bytes = maximum_new_bytes
                .checked_add(source.provider_record_length(locator)?)
                .ok_or(CoreError::ResourceLimit("replication lease bytes"))?;
        }
        let maximum_new_objects = u64::try_from(locator_list.len())
            .map_err(|_| CoreError::ResourceLimit("replication lease objects"))?;
        if maximum_new_objects > 0 {
            for (provider_id, provider) in &online_providers {
                if let Err(error) =
                    provider.begin_backup_write(backup_id, maximum_new_bytes, maximum_new_objects)
                {
                    report.failures.push(ProviderFailure {
                        provider_id: *provider_id,
                        locator: None,
                        reason: error_category(&error).to_owned(),
                    });
                }
            }
            online_providers.retain(|(provider_id, _)| {
                !report
                    .failures
                    .iter()
                    .any(|failure| failure.provider_id == *provider_id && failure.locator.is_none())
            });
        }
        if !locator_list.is_empty() && !online_providers.is_empty() {
            let next = AtomicUsize::new(0);
            let acknowledgements = Mutex::new(report.acknowledgements);
            let failures = Mutex::new(report.failures);
            let worker_count = self.maximum_parallelism.min(online_providers.len());
            std::thread::scope(|scope| {
                for _ in 0..worker_count {
                    let locator_list = &locator_list;
                    let online_providers = &online_providers;
                    let next = &next;
                    let acknowledgements = &acknowledgements;
                    let failures = &failures;
                    scope.spawn(move || {
                        loop {
                            if control.check().is_err() {
                                break;
                            }
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some((provider_id, provider)) = online_providers.get(index) else {
                                break;
                            };
                            let mut cursor = 0;
                            while cursor < locator_list.len() && control.check().is_ok() {
                                let mut batch = Vec::new();
                                let mut batch_bytes = 0_usize;
                                while cursor < locator_list.len()
                                    && batch.len() < MAXIMUM_WRITE_BATCH_RECORDS
                                {
                                    let locator = &locator_list[cursor];
                                    match source.get_provider_record(locator) {
                                        Ok(record)
                                            if batch.is_empty()
                                                || batch_bytes.saturating_add(record.len())
                                                    <= MAXIMUM_WRITE_BATCH_BYTES =>
                                        {
                                            batch_bytes = batch_bytes.saturating_add(record.len());
                                            batch.push((locator.clone(), record));
                                            cursor += 1;
                                        }
                                        Ok(_) => break,
                                        Err(error) => {
                                            if let Ok(mut values) = failures.lock() {
                                                values.push(ProviderFailure {
                                                    provider_id: *provider_id,
                                                    locator: Some(locator.clone()),
                                                    reason: error_category(&error).to_owned(),
                                                });
                                            }
                                            cursor += 1;
                                        }
                                    }
                                }
                                if batch.is_empty() {
                                    continue;
                                }
                                match provider
                                    .put_many_scoped_controlled(backup_id, &batch, control)
                                {
                                    Ok(()) => {
                                        if let Ok(mut values) = acknowledgements.lock() {
                                            values.entry(*provider_id).or_default().extend(
                                                batch.iter().map(|(locator, _)| locator.clone()),
                                            );
                                        }
                                    }
                                    Err(error) => {
                                        let reason = error_category(&error).to_owned();
                                        if let Ok(mut values) = failures.lock() {
                                            values.extend(batch.iter().map(|(locator, _)| {
                                                ProviderFailure {
                                                    provider_id: *provider_id,
                                                    locator: Some(locator.clone()),
                                                    reason: reason.clone(),
                                                }
                                            }));
                                        }
                                    }
                                }
                            }
                        }
                    });
                }
            });
            report.acknowledgements = acknowledgements
                .into_inner()
                .map_err(|_| CoreError::Synchronization)?;
            report.failures = failures
                .into_inner()
                .map_err(|_| CoreError::Synchronization)?;
        }
        if maximum_new_objects > 0 {
            for (provider_id, provider) in &online_providers {
                if let Err(error) = provider.finish_backup_write(backup_id) {
                    report.failures.push(ProviderFailure {
                        provider_id: *provider_id,
                        locator: None,
                        reason: error_category(&error).to_owned(),
                    });
                }
            }
        }
        control.check()?;
        report.failures.sort_by(|left, right| {
            (left.provider_id, &left.locator).cmp(&(right.provider_id, &right.locator))
        });
        Ok(report)
    }

    /// Fetches one authenticated copy without issuing a redundant presence probe.
    pub(crate) fn fetch_plaintext(
        &self,
        reference: &ChunkReference,
        backup_id: BackupId,
        key: &BackupKey,
        allowed_providers: &BTreeSet<DeviceId>,
        control: &JobControl,
    ) -> Result<FetchedChunk, CoreError> {
        let stripe = provider_stripe(&reference.opaque_locator);
        self.fetch_plaintext_striped(
            reference,
            backup_id,
            key,
            allowed_providers,
            control,
            stripe,
        )
    }

    /// Fetches different chunks concurrently while each chunk has only one
    /// provider request in flight. Results retain manifest order.
    pub(crate) fn fetch_plaintexts_parallel(
        &self,
        requests: &[(&ChunkReference, &BTreeSet<DeviceId>)],
        backup_id: BackupId,
        key: &BackupKey,
        control: &JobControl,
        stripe_offset: usize,
    ) -> Result<Vec<FetchedChunk>, CoreError> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        control.check()?;
        let unique_locators = requests
            .iter()
            .map(|(reference, _)| reference.opaque_locator.as_str())
            .collect::<BTreeSet<_>>();
        let batch_ciphertext_bytes = requests
            .iter()
            .map(|(reference, _)| u64::from(reference.ciphertext_length))
            .fold(0_u64, u64::saturating_add);
        let results = Mutex::new(
            std::iter::repeat_with(|| None)
                .take(requests.len())
                .collect::<Vec<Option<Result<FetchedChunk, CoreError>>>>(),
        );
        let batch_failures = Mutex::new(vec![Vec::<ProviderFailure>::new(); requests.len()]);

        // Assign each request to its normal stripe preference, then issue one
        // exact backup-scoped batch per provider. This retains distribution
        // across providers without regressing to unscoped presence probes or
        // one QUIC round trip per chunk.
        if unique_locators.len() == requests.len()
            && requests.len() <= self.maximum_fetch_batch()
            && batch_ciphertext_bytes <= self.maximum_fetch_batch_bytes()
        {
            let maximum_attempts = requests
                .iter()
                .map(|(_, allowed)| {
                    allowed
                        .iter()
                        .filter_map(|id| self.providers.get(id))
                        .filter(|provider| provider.health() == ProviderHealth::Online)
                        .count()
                })
                .max()
                .unwrap_or(0);
            for attempt in 0..maximum_attempts {
                control.check()?;
                let mut grouped = BTreeMap::<DeviceId, (Arc<dyn ChunkProvider>, Vec<usize>)>::new();
                for (index, (_, allowed_providers)) in requests.iter().enumerate() {
                    if results.lock().map_err(|_| CoreError::Synchronization)?[index].is_some() {
                        continue;
                    }
                    let candidates = allowed_providers
                        .iter()
                        .filter_map(|id| self.providers.get(id))
                        .filter(|provider| provider.health() == ProviderHealth::Online)
                        .collect::<Vec<_>>();
                    if attempt >= candidates.len() {
                        continue;
                    }
                    let first = stripe_offset.saturating_add(index) % candidates.len();
                    let provider = candidates[(first + attempt) % candidates.len()];
                    grouped
                        .entry(provider.device_id())
                        .or_insert_with(|| (Arc::clone(provider), Vec::new()))
                        .1
                        .push(index);
                }
                let batches = grouped.into_values().collect::<Vec<_>>();
                let next_batch = AtomicUsize::new(0);
                let worker_count = self.maximum_parallelism.min(batches.len());
                std::thread::scope(|scope| {
                    for _ in 0..worker_count {
                        let next_batch = &next_batch;
                        let results = &results;
                        let batch_failures = &batch_failures;
                        let batches = &batches;
                        scope.spawn(move || {
                            loop {
                                if control.check().is_err() {
                                    break;
                                }
                                let batch_index = next_batch.fetch_add(1, Ordering::Relaxed);
                                let Some((provider, indices)) = batches.get(batch_index) else {
                                    break;
                                };
                                let locators = indices
                                    .iter()
                                    .map(|index| requests[*index].0.opaque_locator.clone())
                                    .collect::<Vec<_>>();
                                let records = match provider
                                    .get_many_controlled(backup_id, &locators, control)
                                {
                                    Ok(records) => records,
                                    Err(error) => {
                                        if let Ok(mut failures) = batch_failures.lock() {
                                            for index in indices {
                                                failures[*index].push(ProviderFailure {
                                                    provider_id: provider.device_id(),
                                                    locator: Some(
                                                        requests[*index].0.opaque_locator.clone(),
                                                    ),
                                                    reason: error_category(&error).to_owned(),
                                                });
                                            }
                                        }
                                        continue;
                                    }
                                };
                                if records.len() != indices.len() {
                                    if let Ok(mut failures) = batch_failures.lock() {
                                        for index in indices {
                                            failures[*index].push(ProviderFailure {
                                                provider_id: provider.device_id(),
                                                locator: Some(
                                                    requests[*index].0.opaque_locator.clone(),
                                                ),
                                                reason: "authentication_failed".to_owned(),
                                            });
                                        }
                                    }
                                    continue;
                                }
                                for ((index, locator), record) in
                                    indices.iter().zip(&locators).zip(records)
                                {
                                    if control.check().is_err() {
                                        break;
                                    }
                                    let reference = requests[*index].0;
                                    if locator != &reference.opaque_locator {
                                        continue;
                                    }
                                    let mut fetched = match decode_fetched_record(
                                        provider.device_id(),
                                        reference,
                                        backup_id,
                                        key,
                                        &record,
                                    ) {
                                        Ok(fetched) => fetched,
                                        Err(error) => {
                                            if let Ok(mut failures) = batch_failures.lock() {
                                                failures[*index].push(ProviderFailure {
                                                    provider_id: provider.device_id(),
                                                    locator: Some(reference.opaque_locator.clone()),
                                                    reason: error_category(&error).to_owned(),
                                                });
                                            }
                                            continue;
                                        }
                                    };
                                    if let Ok(mut failures) = batch_failures.lock() {
                                        fetched.failures = std::mem::take(&mut failures[*index]);
                                    }
                                    if let Ok(mut slots) = results.lock() {
                                        slots[*index] = Some(Ok(fetched));
                                    }
                                }
                            }
                        });
                    }
                });
            }
            control.check()?;
        }

        // A failed provider batch never releases unauthenticated or partial
        // data. Retry only the missing slots through the existing per-chunk
        // multi-provider failover path.
        let next = AtomicUsize::new(0);
        let worker_count = self.maximum_parallelism.min(requests.len());
        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                let next = &next;
                let results = &results;
                let batch_failures = &batch_failures;
                scope.spawn(move || {
                    loop {
                        if control.check().is_err() {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some((reference, allowed_providers)) = requests.get(index) else {
                            break;
                        };
                        if results
                            .lock()
                            .map(|slots| slots[index].is_some())
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        let result = self
                            .fetch_plaintext_striped(
                                reference,
                                backup_id,
                                key,
                                allowed_providers,
                                control,
                                stripe_offset.saturating_add(index),
                            )
                            .map(|mut fetched| {
                                if let Ok(mut failures) = batch_failures.lock() {
                                    let mut retained = std::mem::take(&mut failures[index]);
                                    retained.append(&mut fetched.failures);
                                    fetched.failures = retained;
                                }
                                fetched
                            });
                        if let Ok(mut slots) = results.lock() {
                            slots[index] = Some(result);
                        }
                    }
                });
            }
        });
        control.check()?;
        results
            .into_inner()
            .map_err(|_| CoreError::Synchronization)?
            .into_iter()
            .map(|result| {
                result
                    .ok_or(CoreError::Synchronization)
                    .and_then(std::convert::identity)
            })
            .collect()
    }

    fn fetch_plaintext_striped(
        &self,
        reference: &ChunkReference,
        backup_id: BackupId,
        key: &BackupKey,
        allowed_providers: &BTreeSet<DeviceId>,
        control: &JobControl,
        stripe: usize,
    ) -> Result<FetchedChunk, CoreError> {
        let candidates: Vec<_> = allowed_providers
            .iter()
            .filter_map(|id| self.providers.get(id).map(Arc::clone))
            .filter(|provider| provider.health() == ProviderHealth::Online)
            .collect();
        if candidates.is_empty() {
            return Err(CoreError::ProvidersExhausted(
                reference.opaque_locator.clone(),
            ));
        }
        let mut failures = Vec::new();
        let start = stripe % candidates.len();
        for index in 0..candidates.len() {
            control.check()?;
            let provider = &candidates[(start + index) % candidates.len()];
            let result = provider
                .get_scoped(backup_id, &reference.opaque_locator)
                .and_then(|record| {
                    control.check()?;
                    let encrypted = EncryptedChunk::decode_provider_record(
                        reference.opaque_locator.clone(),
                        reference.plaintext_digest.clone(),
                        &record,
                        reference.plaintext_length as usize,
                    )?;
                    if encrypted.plaintext_length != reference.plaintext_length
                        || encrypted.ciphertext_length() != reference.ciphertext_length
                    {
                        return Err(CoreError::CorruptChunk(reference.opaque_locator.clone()));
                    }
                    key.decrypt_chunk(backup_id, &reference.plaintext_digest, &encrypted)
                });
            match result {
                Ok(plaintext) => {
                    return Ok(FetchedChunk {
                        provider_id: provider.device_id(),
                        plaintext,
                        failures,
                    });
                }
                Err(error) => failures.push(ProviderFailure {
                    provider_id: provider.device_id(),
                    locator: Some(reference.opaque_locator.clone()),
                    reason: error_category(&error).to_owned(),
                }),
            }
        }
        Err(CoreError::ProvidersExhausted(
            reference.opaque_locator.clone(),
        ))
    }
}

fn merge_replication_report(target: &mut ReplicationReport, mut part: ReplicationReport) {
    for (provider_id, locators) in part.acknowledgements {
        target
            .acknowledgements
            .entry(provider_id)
            .or_default()
            .extend(locators);
    }
    target
        .recovery_catalog_acknowledgements
        .append(&mut part.recovery_catalog_acknowledgements);
    for (provider_id, health) in part.provider_health {
        target
            .provider_health
            .entry(provider_id)
            .and_modify(|current| {
                if provider_health_rank(health) > provider_health_rank(*current) {
                    *current = health;
                }
            })
            .or_insert(health);
    }
    target.failures.append(&mut part.failures);
    target.failures.sort_by(|left, right| {
        (left.provider_id, &left.locator, &left.reason).cmp(&(
            right.provider_id,
            &right.locator,
            &right.reason,
        ))
    });
}

const fn provider_health_rank(health: ProviderHealth) -> u8 {
    match health {
        ProviderHealth::Online => 0,
        ProviderHealth::Offline => 1,
        ProviderHealth::Corrupt => 2,
    }
}

fn verify_provider_record(
    reference: &ChunkReference,
    backup_id: BackupId,
    key: &BackupKey,
    record: &[u8],
) -> Result<(), CoreError> {
    let encrypted = EncryptedChunk::decode_provider_record(
        reference.opaque_locator.clone(),
        reference.plaintext_digest.clone(),
        record,
        reference.plaintext_length as usize,
    )?;
    if encrypted.plaintext_length != reference.plaintext_length
        || encrypted.ciphertext_length() != reference.ciphertext_length
    {
        return Err(CoreError::CorruptChunk(reference.opaque_locator.clone()));
    }
    key.decrypt_chunk(backup_id, &reference.plaintext_digest, &encrypted)?;
    Ok(())
}

fn decode_fetched_record(
    provider_id: DeviceId,
    reference: &ChunkReference,
    backup_id: BackupId,
    key: &BackupKey,
    record: &[u8],
) -> Result<FetchedChunk, CoreError> {
    let encrypted = EncryptedChunk::decode_provider_record(
        reference.opaque_locator.clone(),
        reference.plaintext_digest.clone(),
        record,
        reference.plaintext_length as usize,
    )?;
    if encrypted.plaintext_length != reference.plaintext_length
        || encrypted.ciphertext_length() != reference.ciphertext_length
    {
        return Err(CoreError::CorruptChunk(reference.opaque_locator.clone()));
    }
    let plaintext = key.decrypt_chunk(backup_id, &reference.plaintext_digest, &encrypted)?;
    Ok(FetchedChunk {
        provider_id,
        plaintext,
        failures: Vec::new(),
    })
}

impl fmt::Debug for ReplicationScheduler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReplicationScheduler")
            .field("provider_ids", &self.providers.keys().collect::<Vec<_>>())
            .field("maximum_parallelism", &self.maximum_parallelism)
            .finish()
    }
}

pub(crate) struct FetchedChunk {
    pub provider_id: DeviceId,
    pub plaintext: Zeroizing<Vec<u8>>,
    pub failures: Vec<ProviderFailure>,
}

fn error_category(error: &CoreError) -> &'static str {
    match error {
        CoreError::MissingChunk(_) => "missing_chunk",
        CoreError::CorruptChunk(_) | CoreError::AuthenticationFailed => "corrupt_chunk",
        CoreError::ResourceLimit(_) => "resource_limit",
        _ => "provider_error",
    }
}

fn provider_stripe(locator: &str) -> usize {
    let digest = blake3::hash(locator.as_bytes());
    usize::from_be_bytes(
        digest.as_bytes()[..std::mem::size_of::<usize>()]
            .try_into()
            .expect("native word slice"),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use super::*;

    /// `replicate` replicates chunks only; the owner recovery capsule is replicated
    /// separately by the engine, so `ReplicationReport::is_complete` cannot hold for
    /// a bare `replicate` report. Assert the completeness this entry point does
    /// establish: no failures, every selected provider online, every object acked.
    fn chunk_replication_is_complete(report: &ReplicationReport, required_objects: usize) -> bool {
        report.failures.is_empty()
            && report
                .provider_health
                .values()
                .all(|health| *health == ProviderHealth::Online)
            && report
                .acknowledgements
                .values()
                .all(|locators| locators.len() == required_objects)
    }

    struct ToggleProvider {
        id: DeviceId,
        store: ChunkStore,
        online: AtomicBool,
    }

    struct ConcurrentPutProvider {
        id: DeviceId,
        store: ChunkStore,
        active: Arc<AtomicUsize>,
        maximum_active: Arc<AtomicUsize>,
    }

    struct CountingReadProvider {
        id: DeviceId,
        store: ChunkStore,
        active: Arc<AtomicUsize>,
        maximum_active: Arc<AtomicUsize>,
        gets: AtomicUsize,
        batches: AtomicUsize,
        batch_scopes: Mutex<Vec<BackupId>>,
        contains: AtomicUsize,
        cancel_on_get: Option<JobControl>,
    }

    struct CancellingPutProvider {
        id: DeviceId,
        store: ChunkStore,
        control: JobControl,
        puts: AtomicUsize,
    }

    struct StalledReadProvider {
        id: DeviceId,
        batch_started: AtomicBool,
    }

    struct CorruptBatchProvider {
        id: DeviceId,
    }

    impl ChunkProvider for ConcurrentPutProvider {
        fn device_id(&self) -> DeviceId {
            self.id
        }

        fn health(&self) -> ProviderHealth {
            ProviderHealth::Online
        }

        fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError> {
            let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
            self.maximum_active.fetch_max(active, Ordering::AcqRel);
            std::thread::sleep(Duration::from_millis(50));
            let result = self.store.put_provider_record(locator, record).map(|_| ());
            self.active.fetch_sub(1, Ordering::AcqRel);
            result
        }

        fn get(&self, locator: &str) -> Result<Vec<u8>, CoreError> {
            self.store.get_provider_record(locator)
        }

        fn contains(&self, locator: &str) -> Result<bool, CoreError> {
            self.store.contains(locator)
        }
    }

    impl ChunkProvider for CountingReadProvider {
        fn device_id(&self) -> DeviceId {
            self.id
        }

        fn health(&self) -> ProviderHealth {
            ProviderHealth::Online
        }

        fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError> {
            self.store.put_provider_record(locator, record).map(|_| ())
        }

        fn get(&self, locator: &str) -> Result<Vec<u8>, CoreError> {
            let call = self.gets.fetch_add(1, Ordering::AcqRel);
            if call == 0
                && let Some(control) = &self.cancel_on_get
            {
                control.cancel();
            }
            let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
            self.maximum_active.fetch_max(active, Ordering::AcqRel);
            std::thread::sleep(Duration::from_millis(20));
            let result = self.store.get_provider_record(locator);
            self.active.fetch_sub(1, Ordering::AcqRel);
            result
        }

        fn get_many_controlled(
            &self,
            backup_id: BackupId,
            locators: &[String],
            control: &JobControl,
        ) -> Result<Vec<Vec<u8>>, CoreError> {
            self.batches.fetch_add(1, Ordering::AcqRel);
            self.batch_scopes
                .lock()
                .map_err(|_| CoreError::Synchronization)?
                .push(backup_id);
            let mut records = Vec::with_capacity(locators.len());
            for locator in locators {
                control.check()?;
                records.push(self.get(locator)?);
            }
            control.check()?;
            Ok(records)
        }

        fn contains(&self, locator: &str) -> Result<bool, CoreError> {
            self.contains.fetch_add(1, Ordering::AcqRel);
            self.store.contains(locator)
        }
    }

    impl ChunkProvider for CancellingPutProvider {
        fn device_id(&self) -> DeviceId {
            self.id
        }

        fn health(&self) -> ProviderHealth {
            ProviderHealth::Online
        }

        fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError> {
            self.puts.fetch_add(1, Ordering::AcqRel);
            self.control.cancel();
            self.store.put_provider_record(locator, record).map(|_| ())
        }

        fn get(&self, locator: &str) -> Result<Vec<u8>, CoreError> {
            self.store.get_provider_record(locator)
        }

        fn contains(&self, locator: &str) -> Result<bool, CoreError> {
            self.store.contains(locator)
        }
    }

    impl ChunkProvider for StalledReadProvider {
        fn device_id(&self) -> DeviceId {
            self.id
        }

        fn health(&self) -> ProviderHealth {
            ProviderHealth::Online
        }

        fn put(&self, _locator: &str, _record: &[u8]) -> Result<(), CoreError> {
            Err(CoreError::InvalidState("stalled test provider".to_owned()))
        }

        fn get(&self, _locator: &str) -> Result<Vec<u8>, CoreError> {
            panic!("the stalled provider must use the controlled batch boundary")
        }

        fn get_many_controlled(
            &self,
            _backup_id: BackupId,
            _locators: &[String],
            control: &JobControl,
        ) -> Result<Vec<Vec<u8>>, CoreError> {
            self.batch_started.store(true, Ordering::Release);
            loop {
                control.check()?;
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn contains(&self, _locator: &str) -> Result<bool, CoreError> {
            Ok(false)
        }
    }

    impl ChunkProvider for CorruptBatchProvider {
        fn device_id(&self) -> DeviceId {
            self.id
        }

        fn health(&self) -> ProviderHealth {
            ProviderHealth::Online
        }

        fn put(&self, _locator: &str, _record: &[u8]) -> Result<(), CoreError> {
            Err(CoreError::AuthenticationFailed)
        }

        fn get(&self, _locator: &str) -> Result<Vec<u8>, CoreError> {
            Err(CoreError::AuthenticationFailed)
        }

        fn get_many_controlled(
            &self,
            _backup_id: BackupId,
            locators: &[String],
            _control: &JobControl,
        ) -> Result<Vec<Vec<u8>>, CoreError> {
            Ok(locators.iter().map(|_| b"corrupt".to_vec()).collect())
        }

        fn contains(&self, _locator: &str) -> Result<bool, CoreError> {
            Ok(false)
        }
    }

    impl ChunkProvider for ToggleProvider {
        fn device_id(&self) -> DeviceId {
            self.id
        }

        fn health(&self) -> ProviderHealth {
            if self.online.load(Ordering::Relaxed) {
                ProviderHealth::Online
            } else {
                ProviderHealth::Offline
            }
        }

        fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError> {
            self.store.put_provider_record(locator, record).map(|_| ())
        }

        fn get(&self, locator: &str) -> Result<Vec<u8>, CoreError> {
            self.store.get_provider_record(locator)
        }

        fn contains(&self, locator: &str) -> Result<bool, CoreError> {
            self.store.contains(locator)
        }
    }

    #[test]
    fn replication_never_uses_unselected_provider() {
        let source_dir = tempdir().expect("source");
        let selected_dir = tempdir().expect("selected");
        let other_dir = tempdir().expect("other");
        let source = ChunkStore::open(source_dir.path(), 1_048_576).expect("source store");
        let selected = Arc::new(ToggleProvider {
            id: DeviceId::new(),
            store: ChunkStore::open(selected_dir.path(), 1_048_576).expect("provider"),
            online: AtomicBool::new(true),
        });
        let other = Arc::new(ToggleProvider {
            id: DeviceId::new(),
            store: ChunkStore::open(other_dir.path(), 1_048_576).expect("provider"),
            online: AtomicBool::new(true),
        });
        let scheduler = ReplicationScheduler::new(
            [
                Arc::clone(&selected) as Arc<dyn ChunkProvider>,
                Arc::clone(&other) as Arc<dyn ChunkProvider>,
            ],
            4,
        )
        .expect("scheduler");
        let key = BackupKey::generate();
        let backup = BackupId::new();
        let chunk = key.encrypt_chunk(backup, 1, b"data").expect("chunk");
        source.put(&chunk).expect("put");
        let report = scheduler
            .replicate(
                &source,
                &ReplicaIntent::explicit([selected.id]),
                &BTreeSet::from([chunk.opaque_locator.clone()]),
            )
            .expect("replicate");
        assert!(chunk_replication_is_complete(&report, 1));
        assert!(
            selected
                .store
                .contains(&chunk.opaque_locator)
                .expect("selected")
        );
        assert!(!other.store.contains(&chunk.opaque_locator).expect("other"));
    }

    #[test]
    fn replication_parallelizes_across_selected_providers() {
        let source_dir = tempdir().expect("source");
        let first_dir = tempdir().expect("first");
        let second_dir = tempdir().expect("second");
        let source = ChunkStore::open(source_dir.path(), 1_048_576).expect("source store");
        let active = Arc::new(AtomicUsize::new(0));
        let maximum_active = Arc::new(AtomicUsize::new(0));
        let provider = |id, store| {
            Arc::new(ConcurrentPutProvider {
                id,
                store,
                active: Arc::clone(&active),
                maximum_active: Arc::clone(&maximum_active),
            }) as Arc<dyn ChunkProvider>
        };
        let first_id = DeviceId::new();
        let second_id = DeviceId::new();
        let scheduler = ReplicationScheduler::new(
            [
                provider(
                    first_id,
                    ChunkStore::open(first_dir.path(), 1_048_576).expect("first store"),
                ),
                provider(
                    second_id,
                    ChunkStore::open(second_dir.path(), 1_048_576).expect("second store"),
                ),
            ],
            2,
        )
        .expect("scheduler");
        let key = BackupKey::generate();
        let backup = BackupId::new();
        let chunk = key.encrypt_chunk(backup, 1, b"parallel").expect("chunk");
        source.put(&chunk).expect("put");
        let report = scheduler
            .replicate(
                &source,
                &ReplicaIntent::explicit([first_id, second_id]),
                &BTreeSet::from([chunk.opaque_locator]),
            )
            .expect("replicate");
        assert!(chunk_replication_is_complete(&report, 1));
        assert_eq!(maximum_active.load(Ordering::Acquire), 2);
    }

    #[test]
    fn restore_reads_are_single_request_striped_and_bounded_across_chunks() {
        let first_dir = tempdir().expect("first");
        let second_dir = tempdir().expect("second");
        let active = Arc::new(AtomicUsize::new(0));
        let maximum_active = Arc::new(AtomicUsize::new(0));
        let provider = |id, store| {
            Arc::new(CountingReadProvider {
                id,
                store,
                active: Arc::clone(&active),
                maximum_active: Arc::clone(&maximum_active),
                gets: AtomicUsize::new(0),
                batches: AtomicUsize::new(0),
                batch_scopes: Mutex::new(Vec::new()),
                contains: AtomicUsize::new(0),
                cancel_on_get: None,
            })
        };
        let first = provider(
            DeviceId::new(),
            ChunkStore::open(first_dir.path(), 1_048_576).expect("first store"),
        );
        let second = provider(
            DeviceId::new(),
            ChunkStore::open(second_dir.path(), 1_048_576).expect("second store"),
        );
        let scheduler = ReplicationScheduler::new(
            [
                Arc::clone(&first) as Arc<dyn ChunkProvider>,
                Arc::clone(&second) as Arc<dyn ChunkProvider>,
            ],
            4,
        )
        .expect("scheduler");
        let backup_id = BackupId::new();
        let key = BackupKey::generate();
        let mut references = Vec::new();
        for index in 0..8 {
            let plaintext = format!("parallel restore chunk {index}");
            let chunk = key
                .encrypt_chunk(backup_id, 1, plaintext.as_bytes())
                .expect("chunk");
            first.store.put(&chunk).expect("first copy");
            second.store.put(&chunk).expect("second copy");
            references.push(ChunkReference {
                plaintext_digest: chunk.plaintext_digest.clone(),
                opaque_locator: chunk.opaque_locator.clone(),
                plaintext_length: chunk.plaintext_length,
                ciphertext_length: chunk.ciphertext_length(),
            });
        }
        let allowed = BTreeSet::from([first.id, second.id]);
        let requests: Vec<_> = references
            .iter()
            .map(|reference| (reference, &allowed))
            .collect();
        let fetched = scheduler
            .fetch_plaintexts_parallel(&requests, backup_id, &key, &JobControl::new(), 0)
            .expect("fetch");
        let provider_counts = fetched
            .into_iter()
            .fold(BTreeMap::new(), |mut counts, chunk| {
                *counts.entry(chunk.provider_id).or_insert(0_usize) += 1;
                counts
            });

        assert_eq!(first.contains.load(Ordering::Acquire), 0);
        assert_eq!(second.contains.load(Ordering::Acquire), 0);
        assert_eq!(first.gets.load(Ordering::Acquire), 4);
        assert_eq!(second.gets.load(Ordering::Acquire), 4);
        assert_eq!(first.batches.load(Ordering::Acquire), 1);
        assert_eq!(second.batches.load(Ordering::Acquire), 1);
        assert_eq!(
            *first.batch_scopes.lock().expect("first scopes"),
            vec![backup_id]
        );
        assert_eq!(
            *second.batch_scopes.lock().expect("second scopes"),
            vec![backup_id]
        );
        assert_eq!(provider_counts.get(&first.id), Some(&4));
        assert_eq!(provider_counts.get(&second.id), Some(&4));
        assert!((2..=4).contains(&maximum_active.load(Ordering::Acquire)));
    }

    #[test]
    fn cancellation_stops_new_restore_and_replication_requests() {
        let provider_dir = tempdir().expect("provider");
        let source_dir = tempdir().expect("source");
        let backup_id = BackupId::new();
        let key = BackupKey::generate();
        let control = JobControl::new();
        let read_provider = Arc::new(CountingReadProvider {
            id: DeviceId::new(),
            store: ChunkStore::open(provider_dir.path(), 1_048_576).expect("provider store"),
            active: Arc::new(AtomicUsize::new(0)),
            maximum_active: Arc::new(AtomicUsize::new(0)),
            gets: AtomicUsize::new(0),
            batches: AtomicUsize::new(0),
            batch_scopes: Mutex::new(Vec::new()),
            contains: AtomicUsize::new(0),
            cancel_on_get: Some(control.clone()),
        });
        let mut references = Vec::new();
        for index in 0..8 {
            let chunk = key
                .encrypt_chunk(backup_id, 1, format!("cancel {index}").as_bytes())
                .expect("chunk");
            read_provider.store.put(&chunk).expect("provider copy");
            references.push(ChunkReference {
                plaintext_digest: chunk.plaintext_digest.clone(),
                opaque_locator: chunk.opaque_locator.clone(),
                plaintext_length: chunk.plaintext_length,
                ciphertext_length: chunk.ciphertext_length(),
            });
        }
        let allowed = BTreeSet::from([read_provider.id]);
        let requests: Vec<_> = references
            .iter()
            .map(|reference| (reference, &allowed))
            .collect();
        let scheduler =
            ReplicationScheduler::new([Arc::clone(&read_provider) as Arc<dyn ChunkProvider>], 1)
                .expect("scheduler");
        assert!(matches!(
            scheduler.fetch_plaintexts_parallel(&requests, backup_id, &key, &control, 0),
            Err(CoreError::Cancelled)
        ));
        assert_eq!(read_provider.gets.load(Ordering::Acquire), 1);

        let source = ChunkStore::open(source_dir.path(), 1_048_576).expect("source store");
        let replication_control = JobControl::new();
        let write_provider = Arc::new(CancellingPutProvider {
            id: DeviceId::new(),
            store: ChunkStore::open(tempdir().expect("write provider").keep(), 1_048_576)
                .expect("write provider store"),
            control: replication_control.clone(),
            puts: AtomicUsize::new(0),
        });
        let mut locators = BTreeSet::new();
        for index in 0..8 {
            let chunk = key
                .encrypt_chunk(backup_id, 1, format!("replicate cancel {index}").as_bytes())
                .expect("chunk");
            source.put(&chunk).expect("source copy");
            locators.insert(chunk.opaque_locator);
        }
        let scheduler =
            ReplicationScheduler::new([Arc::clone(&write_provider) as Arc<dyn ChunkProvider>], 1)
                .expect("scheduler");
        assert!(matches!(
            scheduler.replicate_controlled(
                &source,
                &ReplicaIntent::explicit([write_provider.id]),
                &locators,
                &replication_control,
                backup_id,
            ),
            Err(CoreError::Cancelled)
        ));
        assert_eq!(write_provider.puts.load(Ordering::Acquire), 1);

        let pipeline_control = JobControl::new();
        let pipeline_provider = Arc::new(CancellingPutProvider {
            id: DeviceId::new(),
            store: ChunkStore::open(tempdir().expect("pipeline provider").keep(), 1_048_576)
                .expect("pipeline provider store"),
            control: pipeline_control.clone(),
            puts: AtomicUsize::new(0),
        });
        let pipeline_scheduler = ReplicationScheduler::new(
            [Arc::clone(&pipeline_provider) as Arc<dyn ChunkProvider>],
            1,
        )
        .expect("pipeline scheduler");
        let pipeline = pipeline_scheduler.start_pipeline(
            source,
            ReplicaIntent::explicit([pipeline_provider.id]),
            pipeline_control,
            backup_id,
        );
        pipeline
            .submit(&locators.into_iter().collect::<Vec<_>>())
            .expect("submit pipeline locators");
        assert!(matches!(pipeline.finish(), Err(CoreError::Cancelled)));
        assert_eq!(pipeline_provider.puts.load(Ordering::Acquire), 1);
    }

    #[test]
    fn corrupt_scoped_batch_fails_over_to_another_authorized_provider() {
        let healthy_dir = tempdir().expect("healthy");
        let mut provider_ids = [DeviceId::new(), DeviceId::new()];
        provider_ids.sort();
        let corrupt = Arc::new(CorruptBatchProvider {
            id: provider_ids[0],
        });
        let healthy = Arc::new(StoreProvider::new(
            provider_ids[1],
            ChunkStore::open(healthy_dir.path(), 1_048_576).expect("healthy store"),
        ));
        let backup_id = BackupId::new();
        let key = BackupKey::generate();
        let chunk = key
            .encrypt_chunk(backup_id, 1, b"healthy failover")
            .expect("chunk");
        healthy.store.put(&chunk).expect("healthy copy");
        let ciphertext_length = chunk.ciphertext_length();
        let reference = ChunkReference {
            plaintext_digest: chunk.plaintext_digest,
            opaque_locator: chunk.opaque_locator,
            plaintext_length: chunk.plaintext_length,
            ciphertext_length,
        };
        let allowed = BTreeSet::from(provider_ids);
        let scheduler = ReplicationScheduler::new(
            [
                corrupt as Arc<dyn ChunkProvider>,
                Arc::clone(&healthy) as Arc<dyn ChunkProvider>,
            ],
            2,
        )
        .expect("scheduler");
        let fetched = scheduler
            .fetch_plaintexts_parallel(
                &[(&reference, &allowed)],
                backup_id,
                &key,
                &JobControl::new(),
                0,
            )
            .expect("fallback");
        assert_eq!(fetched[0].provider_id, healthy.device_id());
        assert_eq!(&fetched[0].plaintext[..], b"healthy failover");
        assert_eq!(fetched[0].failures.len(), 1);
        assert_eq!(fetched[0].failures[0].provider_id, provider_ids[0]);
    }

    #[test]
    fn stalled_provider_restore_fetch_cancels_within_five_seconds() {
        let backup_id = BackupId::new();
        let key = BackupKey::generate();
        let chunk = key
            .encrypt_chunk(backup_id, 1, b"stalled provider cancellation")
            .expect("chunk");
        let ciphertext_length = chunk.ciphertext_length();
        let reference = ChunkReference {
            plaintext_digest: chunk.plaintext_digest,
            opaque_locator: chunk.opaque_locator,
            plaintext_length: chunk.plaintext_length,
            ciphertext_length,
        };
        let provider = Arc::new(StalledReadProvider {
            id: DeviceId::new(),
            batch_started: AtomicBool::new(false),
        });
        let scheduler =
            ReplicationScheduler::new([Arc::clone(&provider) as Arc<dyn ChunkProvider>], 1)
                .expect("scheduler");
        let allowed = BTreeSet::from([provider.id]);
        let control = JobControl::new();
        let canceller = std::thread::spawn({
            let provider = Arc::clone(&provider);
            let control = control.clone();
            move || {
                while !provider.batch_started.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                control.cancel();
            }
        });
        let started = Instant::now();
        let result = scheduler.fetch_plaintexts_parallel(
            &[(&reference, &allowed)],
            backup_id,
            &key,
            &control,
            0,
        );
        canceller.join().expect("canceller");
        assert!(matches!(result, Err(CoreError::Cancelled)));
        assert!(
            started.elapsed() <= Duration::from_secs(5),
            "stalled provider cancellation took {:?}",
            started.elapsed()
        );
    }
}
