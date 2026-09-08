//! Private-process adapter for the pinned maintained folder-sync engine.
//!
//! Verified native/container packages opt in through `NodeRuntimeConfig`.
//! Engine control stays on an authenticated, owner-only Unix socket; native
//! clients must use Covalent's own authorization and folder-sharing workflow.

mod client;
pub mod config;
mod controller;
mod health;
mod host;
mod identity;
mod installation;
#[cfg(target_os = "macos")]
mod mac_host;
mod recovery;
mod service;
#[cfg(test)]
mod service_tests;
mod sharing;
mod state;
mod supervisor;

pub use client::{EngineApiClient, EngineApiError, EngineEndpoint};
pub use health::{FolderHealth, FolderHealthError, FolderLifecycle, collect_folder_health};
pub use host::FolderSyncRuntimeConfig;
pub(crate) use host::FolderSyncRuntimeState;
pub use identity::{EngineIdentity, EngineIdentityError};

pub use supervisor::{
    EngineSupervisorError, OwnedEngineWorker, StopOutcome, VerifiedEngineExecutable,
};

pub use controller::{EngineSessionError, EngineSessionSettings, ManagedEngineSession};
pub use installation::{EngineInstallation, EngineInstallationError};
#[cfg(target_os = "macos")]
pub use mac_host::{MacHostError, discover_packaged_engine};
pub use recovery::{
    MAX_RECOVERY_FILE_BYTES, RecoverAsCopyRequest, RecoveryError, RecoveryReceipt, SelectedVersion,
    SimpleVersionerConfig, VersionMetadata, recover_selected_version_as_copy,
};
pub use service::{
    CommittedMutation, FolderHealthFreshness, FolderSyncIssue, FolderSyncLifecycle,
    FolderSyncService, FolderSyncServiceError, FolderSyncStatus,
};
pub use sharing::{
    FolderShareDelivery, FolderShareRecord, FolderSharingJournal, ShareSummary, SharingError,
    SharingPhase,
};
pub use state::{EngineStateError, EngineStateStore};
