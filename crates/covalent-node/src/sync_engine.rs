//! Private-process adapter for the pinned maintained folder-sync engine.
//!
//! Verified native/container packages opt in through `NodeRuntimeConfig`.
//! Engine control uses an owner-only Unix socket or pinned loopback TLS; native
//! clients must use Covalent's own authorization and folder-sharing workflow.

mod client;
pub mod config;
mod connection;
#[cfg(test)]
mod connection_tests;
mod controller;
#[cfg(test)]
mod controller_tests;
mod health;
mod host;
mod identity;
mod installation;
#[cfg(any(target_os = "linux", test))]
mod linux_host;
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
pub use connection::{
    EnginePeerConnection, EnginePeerConnectionState, PeerConnectionError, collect_peer_connections,
};
pub use health::{FolderHealth, FolderHealthError, FolderLifecycle, collect_folder_health};
pub use host::FolderSyncRuntimeConfig;
pub(crate) use host::FolderSyncRuntimeState;
pub use identity::{EngineIdentity, EngineIdentityError};

pub use supervisor::{
    EngineSupervisorError, OwnedEngineWorker, StopOutcome, VerifiedEngineExecutable,
};

pub use controller::{EngineSessionError, EngineSessionSettings, ManagedEngineSession};
pub use installation::{EngineInstallation, EngineInstallationError};
#[cfg(any(target_os = "linux", test))]
pub use linux_host::{LinuxHostError, discover_packaged_engine as discover_packaged_linux_engine};
#[cfg(target_os = "macos")]
pub use mac_host::{MacHostError, discover_packaged_engine};
pub use recovery::{
    MAX_RECOVERY_FILE_BYTES, RecoverAsCopyRequest, RecoveryError, RecoveryReceipt, SelectedVersion,
    SimpleVersionerConfig, VersionMetadata, recover_selected_version_as_copy,
};
pub use service::{
    CommittedMutation, FolderHealthFreshness, FolderSyncIssue, FolderSyncLifecycle,
    FolderSyncService, FolderSyncServiceError, FolderSyncStatus, PeerConnectionFreshness,
    PeerConnectionState,
};
pub use sharing::{
    FolderShareDelivery, FolderShareRecord, FolderSharingJournal, ShareSummary, SharingError,
    SharingPhase,
};
pub use state::{EngineStateError, EngineStateStore};
