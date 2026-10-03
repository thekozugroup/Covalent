use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use covalent_core::{CoreError, Engine, JobControl, JobState, RecoveryImportReport};
use covalent_protocol::{
    DeviceId, MAX_REMEMBERED_BACKUPS, PROTOCOL_VERSION, PeerRole, RecoveredBackupStatus,
    RecoveryPhase, RecoveryProviderFailure, RecoveryStatus,
};
use serde::{Deserialize, Serialize};

use crate::persist_private_file;

const RECOVERY_STATE_SCHEMA_VERSION: u16 = 1;
const MAX_RECOVERY_STATE_BYTES: u64 = 16 * 1_024 * 1_024;
const MAX_RECOVERY_FAILURE_EVIDENCE: usize = 256;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DurableRecoveryState {
    schema_version: u16,
    status: RecoveryStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DurableRecoveryStateRef<'a> {
    schema_version: u16,
    status: &'a RecoveryStatus,
}

pub(crate) struct RecoveryStateStore {
    path: PathBuf,
    status: Mutex<RecoveryStatus>,
    operation: Mutex<()>,
    control: JobControl,
}

impl RecoveryStateStore {
    pub(crate) fn open(path: PathBuf) -> Result<Self, CoreError> {
        let status = match read_private_bounded_optional(&path)? {
            Some(bytes) => {
                let durable: DurableRecoveryState = serde_json::from_slice(&bytes)?;
                if durable.schema_version != RECOVERY_STATE_SCHEMA_VERSION {
                    return Err(CoreError::InvalidState(
                        "unsupported recovery state schema".to_owned(),
                    ));
                }
                validate_status(&durable.status)?;
                durable.status
            }
            None => empty_status(RecoveryPhase::NotConfigured, BTreeSet::new()),
        };
        Ok(Self {
            path,
            status: Mutex::new(status),
            operation: Mutex::new(()),
            control: JobControl::new(),
        })
    }

    pub(crate) fn begin(&self, engine: &Engine) -> Result<(), CoreError> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let configured = configured_provider_ids(engine)?;
        self.replace(empty_status(RecoveryPhase::Pending, configured))
    }

    pub(crate) fn status(&self) -> Result<RecoveryStatus, CoreError> {
        self.status
            .lock()
            .map_err(|_| CoreError::Synchronization)
            .map(|status| status.clone())
    }

    pub(crate) fn should_retry(&self) -> Result<bool, CoreError> {
        Ok(matches!(
            self.status()?.phase,
            RecoveryPhase::Pending
                | RecoveryPhase::Partial
                | RecoveryPhase::Blocked
                | RecoveryPhase::NoCatalogs
        ))
    }

    pub(crate) fn retry(&self, engine: &Engine) -> Result<RecoveryStatus, CoreError> {
        let configured = configured_provider_ids(engine)?;
        self.retry_with(configured, || {
            engine.import_recovery_catalogs_report_controlled(&self.control)
        })
    }

    fn retry_with(
        &self,
        configured: BTreeSet<DeviceId>,
        import: impl FnOnce() -> Result<RecoveryImportReport, CoreError>,
    ) -> Result<RecoveryStatus, CoreError> {
        let _operation = loop {
            self.check_running()?;
            match self.operation.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(CoreError::Synchronization);
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
            }
        };
        self.check_running()?;
        let status = match import() {
            Ok(report) => status_from_report(report, configured.clone()),
            Err(_) => RecoveryStatus {
                protocol_version: PROTOCOL_VERSION,
                phase: RecoveryPhase::Blocked,
                recovered_backups: Vec::new(),
                queried_provider_ids: BTreeSet::new(),
                failures: configured
                    .iter()
                    .copied()
                    .map(|provider_id| RecoveryProviderFailure {
                        provider_id,
                        snapshot_id: None,
                        reason: "recovery_catalog_import_failed".to_owned(),
                    })
                    .collect(),
                configured_provider_ids: configured.clone(),
                newer_snapshot_may_exist: true,
            },
        };
        // A provider-controlled catalog can legitimately contain the maximum
        // number of remembered backups, while its public status representation
        // is too large for the deliberately bounded durable state file. Never
        // leave the previous (possibly optimistic) state in that case: publish
        // compact, conservative evidence instead.
        let status = if status_is_durably_representable(&status) {
            status
        } else {
            resource_limited_status(configured)
        };
        self.replace(status.clone())?;
        Ok(status)
    }

    pub(crate) fn cancel(&self) {
        self.control.cancel();
    }

    fn check_running(&self) -> Result<(), CoreError> {
        match self.control.state() {
            JobState::Running => Ok(()),
            JobState::Paused => Err(CoreError::Paused),
            JobState::Cancelled => Err(CoreError::Cancelled),
        }
    }

    fn replace(&self, status: RecoveryStatus) -> Result<(), CoreError> {
        validate_status(&status)?;
        let bytes = serde_json::to_vec_pretty(&DurableRecoveryStateRef {
            schema_version: RECOVERY_STATE_SCHEMA_VERSION,
            status: &status,
        })?;
        if bytes.len() as u64 > MAX_RECOVERY_STATE_BYTES {
            return Err(CoreError::ResourceLimit("recovery state"));
        }
        persist_private_file(&self.path, &bytes)?;
        *self.status.lock().map_err(|_| CoreError::Synchronization)? = status;
        Ok(())
    }
}

fn validate_status(status: &RecoveryStatus) -> Result<(), CoreError> {
    let backup_ids: BTreeSet<_> = status
        .recovered_backups
        .iter()
        .map(|backup| backup.backup_id)
        .collect();
    if status.protocol_version != PROTOCOL_VERSION
        || status.recovered_backups.len() > 100_000
        || backup_ids.len() != status.recovered_backups.len()
        || !status
            .queried_provider_ids
            .is_subset(&status.configured_provider_ids)
        || status.configured_provider_ids.len() > 128
        || status.queried_provider_ids.len() > 128
        || status.failures.len() > MAX_RECOVERY_FAILURE_EVIDENCE
        || status.recovered_backups.iter().any(|backup| {
            !valid_recovery_identifier(&backup.snapshot_id)
                || backup.source_provider_ids.is_empty()
                || backup.source_provider_ids.len() > 128
                || !backup
                    .source_provider_ids
                    .is_subset(&status.configured_provider_ids)
        })
        || status.failures.iter().any(|failure| {
            !status
                .configured_provider_ids
                .contains(&failure.provider_id)
                || failure.reason.is_empty()
                || !failure
                    .reason
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                || failure.reason.len() > 128
                || failure
                    .snapshot_id
                    .as_deref()
                    .is_some_and(|snapshot_id| !valid_recovery_identifier(snapshot_id))
        })
    {
        return Err(CoreError::InvalidState(
            "unsupported or excessive recovery state".to_owned(),
        ));
    }
    let has_backups = !status.recovered_backups.is_empty();
    let complete = !status.newer_snapshot_may_exist
        && status.failures.is_empty()
        && status.queried_provider_ids == status.configured_provider_ids;
    let consistent = match status.phase {
        RecoveryPhase::NotConfigured => {
            !has_backups && complete && status.configured_provider_ids.is_empty()
        }
        RecoveryPhase::Pending | RecoveryPhase::Blocked => {
            !has_backups && status.newer_snapshot_may_exist
        }
        RecoveryPhase::Imported => has_backups && complete,
        RecoveryPhase::Partial => has_backups && status.newer_snapshot_may_exist,
        RecoveryPhase::NoCatalogs => !has_backups && complete,
    };
    if !consistent {
        return Err(CoreError::InvalidState(
            "inconsistent recovery phase".to_owned(),
        ));
    }
    Ok(())
}

fn valid_recovery_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn status_is_durably_representable(status: &RecoveryStatus) -> bool {
    validate_status(status).is_ok()
        && serde_json::to_vec_pretty(&DurableRecoveryStateRef {
            schema_version: RECOVERY_STATE_SCHEMA_VERSION,
            status,
        })
        .is_ok_and(|bytes| bytes.len() as u64 <= MAX_RECOVERY_STATE_BYTES)
}

fn resource_limited_status(configured_provider_ids: BTreeSet<DeviceId>) -> RecoveryStatus {
    let failures = configured_provider_ids
        .iter()
        .copied()
        .take(MAX_RECOVERY_FAILURE_EVIDENCE)
        .map(|provider_id| RecoveryProviderFailure {
            provider_id,
            snapshot_id: None,
            reason: "recovery_catalog_resource_limit".to_owned(),
        })
        .collect();
    RecoveryStatus {
        protocol_version: PROTOCOL_VERSION,
        phase: RecoveryPhase::Blocked,
        recovered_backups: Vec::new(),
        queried_provider_ids: BTreeSet::new(),
        configured_provider_ids,
        failures,
        newer_snapshot_may_exist: true,
    }
}

fn empty_status(
    phase: RecoveryPhase,
    configured_provider_ids: BTreeSet<DeviceId>,
) -> RecoveryStatus {
    let newer_snapshot_may_exist = phase == RecoveryPhase::Pending;
    RecoveryStatus {
        protocol_version: PROTOCOL_VERSION,
        phase,
        recovered_backups: Vec::new(),
        queried_provider_ids: BTreeSet::new(),
        configured_provider_ids,
        failures: Vec::new(),
        newer_snapshot_may_exist,
    }
}

fn configured_provider_ids(engine: &Engine) -> Result<BTreeSet<DeviceId>, CoreError> {
    Ok(engine
        .config()?
        .trusted_peers
        .into_iter()
        .filter_map(|(id, grant)| {
            (!grant.revoked && grant.roles.contains(&PeerRole::StorageProvider)).then_some(id)
        })
        .collect())
}

fn status_from_report(
    report: RecoveryImportReport,
    configured_provider_ids: BTreeSet<DeviceId>,
) -> RecoveryStatus {
    let queried_provider_ids = report
        .queried_provider_ids
        .intersection(&configured_provider_ids)
        .copied()
        .collect();
    let mut failure_evidence_truncated = false;
    let mut failures = Vec::new();
    for failure in report.failures {
        if !configured_provider_ids.contains(&failure.provider_id) {
            continue;
        }
        if failure
            .locator
            .as_deref()
            .is_some_and(|locator| !valid_recovery_identifier(locator))
            || failure.reason.is_empty()
            || failure.reason.len() > 128
        {
            failure_evidence_truncated = true;
            continue;
        }
        if failures.len() == MAX_RECOVERY_FAILURE_EVIDENCE {
            failure_evidence_truncated = true;
            continue;
        }
        failures.push(RecoveryProviderFailure {
            provider_id: failure.provider_id,
            snapshot_id: failure.locator,
            reason: failure.reason,
        });
    }
    if report.recovered_backups.len() > MAX_REMEMBERED_BACKUPS {
        return resource_limited_status(configured_provider_ids);
    }
    let recovered_backups: Vec<_> = report
        .recovered_backups
        .into_iter()
        .map(|backup| RecoveredBackupStatus {
            backup_id: backup.backup_id,
            snapshot_id: backup.snapshot_id,
            source_provider_ids: backup.source_providers,
        })
        .collect();
    let incomplete = report.newer_snapshot_may_exist
        || failure_evidence_truncated
        || !failures.is_empty()
        || queried_provider_ids != configured_provider_ids;
    let phase = if report.blocked {
        RecoveryPhase::Blocked
    } else if recovered_backups.is_empty() {
        if incomplete {
            RecoveryPhase::Blocked
        } else {
            RecoveryPhase::NoCatalogs
        }
    } else if incomplete {
        RecoveryPhase::Partial
    } else {
        RecoveryPhase::Imported
    };
    RecoveryStatus {
        protocol_version: PROTOCOL_VERSION,
        phase,
        recovered_backups,
        queried_provider_ids,
        configured_provider_ids,
        failures,
        newer_snapshot_may_exist: incomplete,
    }
}

fn read_private_bounded_optional(path: &Path) -> Result<Option<Vec<u8>>, CoreError> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};
        let descriptor = match open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => {
                return Err(CoreError::Io {
                    operation: "open recovery state without following links",
                    path: path.to_path_buf(),
                    source: std::io::Error::from_raw_os_error(error.raw_os_error()),
                });
            }
        };
        let stat = fstat(&descriptor).map_err(|error| CoreError::Io {
            operation: "inspect open recovery state",
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        })?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_mode & 0o077 != 0
            || stat.st_size < 0
            || stat.st_size as u64 > MAX_RECOVERY_STATE_BYTES
        {
            return Err(CoreError::InvalidState(
                "recovery state is not a bounded private regular file".to_owned(),
            ));
        }
        fs::File::from(descriptor)
    };
    #[cfg(not(unix))]
    let file = {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect recovery state",
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_RECOVERY_STATE_BYTES
        {
            return Err(CoreError::InvalidState(
                "recovery state is not a bounded regular file".to_owned(),
            ));
        }
        fs::File::open(path).map_err(|source| CoreError::Io {
            operation: "open recovery state",
            path: path.to_path_buf(),
            source,
        })?
    };
    use std::io::Read as _;
    let mut bytes = Vec::new();
    file.take(MAX_RECOVERY_STATE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| CoreError::Io {
            operation: "read recovery state",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > MAX_RECOVERY_STATE_BYTES {
        return Err(CoreError::ResourceLimit("recovery state"));
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use covalent_core::{ProviderFailure, RecoveredBackup, RecoveryImportReport};
    use covalent_protocol::{BackupId, DeviceId, RecoveryPhase};

    use super::{empty_status, status_from_report};

    #[test]
    fn pending_attempt_never_denies_that_a_newer_snapshot_may_exist() {
        let status = empty_status(RecoveryPhase::Pending, BTreeSet::new());
        assert!(status.newer_snapshot_may_exist);
    }

    #[test]
    fn missing_provider_never_reports_imported_or_no_catalogs() {
        let provider = DeviceId::new();
        let status =
            status_from_report(RecoveryImportReport::default(), BTreeSet::from([provider]));
        assert_eq!(status.phase, RecoveryPhase::Blocked);
        assert!(status.newer_snapshot_may_exist);
    }

    #[test]
    fn partial_report_preserves_exact_sources_and_warning() {
        let provider = DeviceId::new();
        let missing = DeviceId::new();
        let backup_id = BackupId::new();
        let status = status_from_report(
            RecoveryImportReport {
                recovered_backups: vec![covalent_core::RecoveredBackup {
                    backup_id,
                    snapshot_id: "snapshot-1".to_owned(),
                    source_providers: BTreeSet::from([provider]),
                }],
                queried_provider_ids: BTreeSet::from([provider]),
                failures: Vec::new(),
                newer_snapshot_may_exist: true,
                blocked: false,
            },
            BTreeSet::from([provider, missing]),
        );
        assert_eq!(status.phase, RecoveryPhase::Partial);
        assert_eq!(status.recovered_backups[0].backup_id, backup_id);
        assert_eq!(
            status.recovered_backups[0].source_provider_ids,
            BTreeSet::from([provider])
        );
        assert!(status.newer_snapshot_may_exist);
    }

    #[test]
    fn overlapping_retries_are_serialized_through_durable_publication() {
        let directory = tempfile::TempDir::new().expect("directory");
        let store = Arc::new(
            super::RecoveryStateStore::open(directory.path().join("recovery-state.json"))
                .expect("store"),
        );
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first_store = Arc::clone(&store);
        let first = std::thread::spawn(move || {
            first_store
                .retry_with(BTreeSet::new(), || {
                    first_entered_tx.send(()).expect("entered first import");
                    release_first_rx.recv().expect("release first import");
                    Ok(RecoveryImportReport::default())
                })
                .expect("first retry")
        });
        first_entered_rx.recv().expect("first holds operation lock");

        let (second_attempting_tx, second_attempting_rx) = mpsc::channel();
        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let second_store = Arc::clone(&store);
        let second = std::thread::spawn(move || {
            second_attempting_tx.send(()).expect("second attempting");
            second_store
                .retry_with(BTreeSet::new(), || {
                    second_entered_tx.send(()).expect("entered second import");
                    Ok(RecoveryImportReport::default())
                })
                .expect("second retry")
        });
        second_attempting_rx.recv().expect("second thread started");
        assert!(
            second_entered_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "second import must not begin before the first result is durably published"
        );
        release_first_tx.send(()).expect("release first");
        second_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second import proceeds after publication");
        assert_eq!(
            first.join().expect("join first").phase,
            RecoveryPhase::NoCatalogs
        );
        assert_eq!(
            second.join().expect("join second").phase,
            RecoveryPhase::NoCatalogs
        );
    }

    #[test]
    fn excess_failure_evidence_is_bounded_and_remains_conservative() {
        let provider = DeviceId::new();
        let status = status_from_report(
            RecoveryImportReport {
                failures: (0..(super::MAX_RECOVERY_FAILURE_EVIDENCE + 1))
                    .map(|_| ProviderFailure {
                        provider_id: provider,
                        locator: None,
                        reason: "recovery_catalog_provider_offline".to_owned(),
                    })
                    .collect(),
                ..RecoveryImportReport::default()
            },
            BTreeSet::from([provider]),
        );
        assert_eq!(status.failures.len(), super::MAX_RECOVERY_FAILURE_EVIDENCE);
        assert!(status.newer_snapshot_may_exist);
        assert_eq!(status.phase, RecoveryPhase::Blocked);
    }

    #[test]
    fn oversized_public_status_publishes_compact_blocked_evidence() {
        let directory = tempfile::TempDir::new().expect("directory");
        let store = super::RecoveryStateStore::open(directory.path().join("recovery-state.json"))
            .expect("store");
        let report = RecoveryImportReport {
            recovered_backups: (0..covalent_protocol::MAX_REMEMBERED_BACKUPS)
                .map(|_| RecoveredBackup {
                    backup_id: BackupId::new(),
                    snapshot_id: "a".repeat(128),
                    source_providers: BTreeSet::new(),
                })
                .collect(),
            ..RecoveryImportReport::default()
        };
        let status = store
            .retry_with(BTreeSet::new(), || Ok(report))
            .expect("compact status is durable");
        assert_eq!(status.phase, RecoveryPhase::Blocked);
        assert!(status.recovered_backups.is_empty());
        assert!(status.newer_snapshot_may_exist);
        assert_eq!(
            store.status().expect("published status").phase,
            RecoveryPhase::Blocked
        );
    }
    #[test]
    fn cancelled_retry_does_not_wait_for_another_import_or_start_a_new_one() {
        let directory = tempfile::TempDir::new().expect("directory");
        let store = Arc::new(
            super::RecoveryStateStore::open(directory.path().join("recovery-state.json"))
                .expect("store"),
        );
        let operation = store.operation.lock().expect("hold in-flight import lock");
        let (sent, received) = mpsc::channel();
        let worker_store = Arc::clone(&store);
        let worker = std::thread::spawn(move || {
            sent.send(worker_store.retry_with(BTreeSet::new(), || {
                panic!("cancelled import must not start")
            }))
            .expect("result");
        });
        store.cancel();
        assert!(matches!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("bounded cancellation"),
            Err(covalent_core::CoreError::Cancelled)
        ));
        drop(operation);
        worker.join().expect("worker");
        assert!(matches!(
            store.retry_with(BTreeSet::new(), || panic!(
                "shutdown cannot admit another retry"
            )),
            Err(covalent_core::CoreError::Cancelled)
        ));
    }
    #[test]
    fn partial_status_accepts_verified_sources_from_an_incomplete_provider_listing() {
        let provider = DeviceId::new();
        let status = status_from_report(
            RecoveryImportReport {
                recovered_backups: vec![RecoveredBackup {
                    backup_id: BackupId::new(),
                    snapshot_id: "snapshot-1".to_owned(),
                    source_providers: BTreeSet::from([provider]),
                }],
                newer_snapshot_may_exist: true,
                ..RecoveryImportReport::default()
            },
            BTreeSet::from([provider]),
        );
        assert_eq!(status.phase, RecoveryPhase::Partial);
        assert!(status.queried_provider_ids.is_empty());
        super::validate_status(&status).expect("partial streaming evidence remains valid");
        let mut invalid = status;
        invalid.configured_provider_ids.clear();
        assert!(super::validate_status(&invalid).is_err());
    }
}
