//! Per-folder progress and error observations from the owned runtime.

use time::OffsetDateTime;
use uuid::Uuid;

/// Lifecycle values retained by the public folder-status contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderLifecycle {
    Starting,
    Idle,
    Scanning,
    ScanWaiting,
    SyncWaiting,
    SyncPreparing,
    Syncing,
    Cleaning,
    CleanWaiting,
    Error,
}

/// A bounded observation of one configured folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderHealth {
    pub folder: Uuid,
    pub lifecycle: FolderLifecycle,
    /// Time of the runtime observation.
    pub state_changed: OffsetDateTime,
    pub remaining_files: u64,
    pub remaining_bytes: u64,
    /// Number of scan or transfer errors reported by the runtime.
    pub scan_pull_error_count: u64,
    /// Error rows included in this observation.
    pub reported_error_rows: u16,
    pub status_error: bool,
    pub watch_error: bool,
}

impl FolderHealth {
    /// Whether the observation contains an explicit runtime error.
    pub const fn has_reported_errors(&self) -> bool {
        self.scan_pull_error_count != 0
            || self.reported_error_rows != 0
            || self.status_error
            || self.watch_error
    }
}
