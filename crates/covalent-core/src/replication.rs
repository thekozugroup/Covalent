use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use covalent_protocol::{BackupId, ChunkReference, DeviceId, ReplicaIntent};
use zeroize::Zeroizing;

use crate::engine::JobControl;
use crate::{BackupKey, ChunkStore, CoreError, EncryptedChunk, RecoveryCapsule};

const MAXIMUM_FETCH_BATCH_BYTES: u64 = 8 * 1_024 * 1_024;

/// Coarse provider reachability and integrity state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderHealth {
    /// Reachable and accepting requests.
    Online,
    /// Explicitly unavailable.
    Offline,
    /// Returned at least one corrupt object in this operation.
    Corrupt,
}

/// One provider/object failure retained for visible degraded state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFailure {
    /// Provider that failed.
    pub provider_id: DeviceId,
    /// Affected locator, when known.
    pub locator: Option<String>,
    /// Stable non-secret failure category.
    pub reason: String,
}

/// Explicit replication outcome. Missing acknowledgements are never backfilled elsewhere.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
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

    pub(crate) fn verify_provider_copy(
        &self,
        provider_id: DeviceId,
        reference: &ChunkReference,
        backup_id: BackupId,
        key: &BackupKey,
    ) -> Result<(), CoreError> {
        let provider = self
            .providers
            .get(&provider_id)
            .ok_or_else(|| CoreError::ProvidersExhausted(reference.opaque_locator.clone()))?;
        if provider.health() != ProviderHealth::Online {
            return Err(CoreError::ProvidersExhausted(
                reference.opaque_locator.clone(),
            ));
        }
        let record = provider.get_scoped(backup_id, &reference.opaque_locator)?;
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
        key.decrypt_chunk(backup_id, &reference.plaintext_digest, &encrypted)?;
        Ok(())
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
        let next = AtomicUsize::new(0);
        let failures = Mutex::new(Vec::new());
        let worker_count = self.maximum_parallelism.min(copies.len());
        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                let next = &next;
                let failures = &failures;
                scope.spawn(move || {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some((provider_id, reference)) = copies.get(index) else {
                            break;
                        };
                        if let Err(error) =
                            self.verify_provider_copy(*provider_id, reference, backup_id, key)
                            && let Ok(mut values) = failures.lock()
                        {
                            values.push(ProviderFailure {
                                provider_id: *provider_id,
                                locator: Some(reference.opaque_locator.clone()),
                                reason: error_category(&error).to_owned(),
                            });
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
        let job_count = locator_list
            .len()
            .checked_mul(online_providers.len())
            .ok_or(CoreError::ResourceLimit("replication work items"))?;
        if job_count > 0 {
            let next = AtomicUsize::new(0);
            let acknowledgements = Mutex::new(report.acknowledgements);
            let failures = Mutex::new(report.failures);
            let worker_count = self.maximum_parallelism.min(job_count);
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
                            if index >= job_count {
                                break;
                            }
                            let locator = &locator_list[index / online_providers.len()];
                            let (provider_id, provider) =
                                &online_providers[index % online_providers.len()];
                            let result = source.get_provider_record(locator).and_then(|record| {
                                control.check()?;
                                provider.put_scoped(backup_id, locator, &record)
                            });
                            match result {
                                Ok(()) => {
                                    if let Ok(mut values) = acknowledgements.lock() {
                                        values
                                            .entry(*provider_id)
                                            .or_default()
                                            .insert(locator.clone());
                                    }
                                }
                                Err(error) => {
                                    if let Ok(mut values) = failures.lock() {
                                        values.push(ProviderFailure {
                                            provider_id: *provider_id,
                                            locator: Some(locator.clone()),
                                            reason: error_category(&error).to_owned(),
                                        });
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
        let common_providers: Vec<_> = self
            .providers
            .iter()
            .filter(|(provider_id, provider)| {
                provider.health() == ProviderHealth::Online
                    && requests
                        .iter()
                        .all(|(_, allowed)| allowed.contains(provider_id))
            })
            .collect();
        let unique_locators = requests
            .iter()
            .map(|(reference, _)| reference.opaque_locator.as_str())
            .collect::<BTreeSet<_>>();
        let batch_ciphertext_bytes = requests
            .iter()
            .map(|(reference, _)| u64::from(reference.ciphertext_length))
            .fold(0_u64, u64::saturating_add);
        if let [(_, provider)] = common_providers.as_slice()
            && unique_locators.len() == requests.len()
            && requests.len() <= self.maximum_fetch_batch()
            && batch_ciphertext_bytes <= self.maximum_fetch_batch_bytes()
        {
            let locators = requests
                .iter()
                .map(|(reference, _)| reference.opaque_locator.clone())
                .collect::<Vec<_>>();
            match provider.get_many_controlled(backup_id, &locators, control) {
                Ok(records) => {
                    if records.len() != requests.len() {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    let mut fetched = Vec::with_capacity(records.len());
                    for (((reference, _), record), locator) in
                        requests.iter().zip(records).zip(&locators)
                    {
                        control.check()?;
                        if locator != &reference.opaque_locator {
                            return Err(CoreError::AuthenticationFailed);
                        }
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
                        let plaintext =
                            key.decrypt_chunk(backup_id, &reference.plaintext_digest, &encrypted)?;
                        fetched.push(FetchedChunk {
                            provider_id: provider.device_id(),
                            plaintext,
                            failures: Vec::new(),
                        });
                    }
                    return Ok(fetched);
                }
                Err(error @ (CoreError::Paused | CoreError::Cancelled)) => return Err(error),
                Err(_) => {
                    // Preserve multi-source recovery semantics below. A failed
                    // batch never releases a partial response to this caller.
                }
            }
        }
        let next = AtomicUsize::new(0);
        let results = Mutex::new(
            std::iter::repeat_with(|| None)
                .take(requests.len())
                .collect::<Vec<Option<Result<FetchedChunk, CoreError>>>>(),
        );
        let worker_count = self.maximum_parallelism.min(requests.len());
        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                let next = &next;
                let results = &results;
                scope.spawn(move || {
                    loop {
                        if control.check().is_err() {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some((reference, allowed_providers)) = requests.get(index) else {
                            break;
                        };
                        let result = self.fetch_plaintext_striped(
                            reference,
                            backup_id,
                            key,
                            allowed_providers,
                            control,
                            stripe_offset.saturating_add(index),
                        );
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
