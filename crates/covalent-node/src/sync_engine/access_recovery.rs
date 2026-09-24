//! Worker-free access to authenticated folder consent after bookmark loss.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::Mutex;
use uuid::Uuid;

use super::{EngineInstallation, FolderSharingJournal, ShareSummary, SharingError};

/// An existing authenticated share journal with no worker-launch capability.
///
/// This coordinator can only list consent, tombstone a share, or replace one
/// retained local root capability. It owns no executable, process, health task,
/// or network client.
pub struct FolderSyncAccessRecovery {
    journal: Mutex<FolderSharingJournal>,
    _installation: Arc<EngineInstallation>,
}

impl fmt::Debug for FolderSyncAccessRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FolderSyncAccessRecovery([PRIVATE])")
    }
}

impl FolderSyncAccessRecovery {
    pub(super) fn new(
        journal: FolderSharingJournal,
        installation: Arc<EngineInstallation>,
    ) -> Result<Self, SharingError> {
        if journal.binding().engine_device_id.as_str() != installation.device_id().as_str() {
            return Err(SharingError::InvalidState);
        }
        installation
            .revalidate()
            .map_err(|_| SharingError::InvalidState)?;
        Ok(Self {
            journal: Mutex::new(journal),
            _installation: installation,
        })
    }

    /// Read redacted consent summaries from the authenticated durable journal.
    pub async fn summaries(&self) -> Result<Vec<ShareSummary>, SharingError> {
        let mut journal = self
            .journal
            .try_lock()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        journal.reconciled_summaries()
    }

    /// Durably tombstone a share while preserving every user file.
    pub async fn remove(&self, offer_id: Uuid) -> Result<(), SharingError> {
        let mut journal = self
            .journal
            .try_lock()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        journal.remove(offer_id)
    }

    /// Durably replace one retained local root. No worker is launched here.
    pub async fn repair_root(
        &self,
        offer_id: Uuid,
        selected_root: &Path,
    ) -> Result<(), SharingError> {
        let mut journal = self
            .journal
            .try_lock()
            .map_err(|_| SharingError::PersistenceUncertain)?;
        journal.repair_root(offer_id, selected_root)
    }
}
