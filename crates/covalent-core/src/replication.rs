use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use covalent_protocol::{BackupId, ChunkReference, DeviceId, ReplicaIntent};
use zeroize::Zeroizing;

use crate::{BackupKey, ChunkStore, CoreError, EncryptedChunk};

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
    }
}

/// Authorized encrypted storage provider boundary.
pub trait ChunkProvider: Send + Sync {
    /// Stable paired device identity.
    fn device_id(&self) -> DeviceId;
    /// Current transport reachability.
    fn health(&self) -> ProviderHealth;
    /// Durably stores one opaque encrypted record.
    fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError>;
    /// Fetches one bounded opaque encrypted record.
    fn get(&self, locator: &str) -> Result<Vec<u8>, CoreError>;
    /// Returns whether this provider currently advertises the object.
    fn contains(&self, locator: &str) -> Result<bool, CoreError>;
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
        if !provider.contains(&reference.opaque_locator)? {
            return Err(CoreError::MissingChunk(reference.opaque_locator.clone()));
        }
        let record = provider.get(&reference.opaque_locator)?;
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
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            if index >= job_count {
                                break;
                            }
                            let locator = &locator_list[index / online_providers.len()];
                            let (provider_id, provider) =
                                &online_providers[index % online_providers.len()];
                            let result = source
                                .get_provider_record(locator)
                                .and_then(|record| provider.put(locator, &record));
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
        report.failures.sort_by(|left, right| {
            (left.provider_id, &left.locator).cmp(&(right.provider_id, &right.locator))
        });
        Ok(report)
    }

    /// Fetches from all allowed, connected providers in parallel; first valid copy wins.
    pub(crate) fn fetch_plaintext(
        &self,
        reference: &ChunkReference,
        backup_id: BackupId,
        key: &BackupKey,
        allowed_providers: &BTreeSet<DeviceId>,
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
        let found: Mutex<Option<(DeviceId, Zeroizing<Vec<u8>>)>> = Mutex::new(None);
        let failures = Mutex::new(Vec::new());
        let next = AtomicUsize::new(0);
        let worker_count = self.maximum_parallelism.min(candidates.len());
        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                let found = &found;
                let failures = &failures;
                let next = &next;
                let candidates = &candidates;
                scope.spawn(move || {
                    loop {
                        if found.lock().is_ok_and(|value| value.is_some()) {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(provider) = candidates.get(index) else {
                            break;
                        };
                        let result = provider
                            .contains(&reference.opaque_locator)
                            .and_then(|present| {
                                if present {
                                    provider.get(&reference.opaque_locator)
                                } else {
                                    Err(CoreError::MissingChunk(reference.opaque_locator.clone()))
                                }
                            })
                            .and_then(|record| {
                                let encrypted = EncryptedChunk::decode_provider_record(
                                    reference.opaque_locator.clone(),
                                    reference.plaintext_digest.clone(),
                                    &record,
                                    reference.plaintext_length as usize,
                                )?;
                                if encrypted.plaintext_length != reference.plaintext_length
                                    || encrypted.ciphertext_length() != reference.ciphertext_length
                                {
                                    return Err(CoreError::CorruptChunk(
                                        reference.opaque_locator.clone(),
                                    ));
                                }
                                key.decrypt_chunk(
                                    backup_id,
                                    &reference.plaintext_digest,
                                    &encrypted,
                                )
                            });
                        match result {
                            Ok(plaintext) => {
                                if let Ok(mut slot) = found.lock()
                                    && slot.is_none()
                                {
                                    *slot = Some((provider.device_id(), plaintext));
                                }
                            }
                            Err(error) => {
                                if let Ok(mut values) = failures.lock() {
                                    values.push(ProviderFailure {
                                        provider_id: provider.device_id(),
                                        locator: Some(reference.opaque_locator.clone()),
                                        reason: error_category(&error).to_owned(),
                                    });
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
        failures.sort_by_key(|failure| failure.provider_id);
        let (provider_id, plaintext) = found
            .into_inner()
            .map_err(|_| CoreError::Synchronization)?
            .ok_or_else(|| CoreError::ProvidersExhausted(reference.opaque_locator.clone()))?;
        Ok(FetchedChunk {
            provider_id,
            plaintext,
            failures,
        })
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;

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
        assert!(report.is_complete(1));
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
        assert!(report.is_complete(1));
        assert_eq!(maximum_active.load(Ordering::Acquire), 2);
    }
}
