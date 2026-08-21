//! Memory-safe Covalent backup, storage, verification, restore, and trust engine.
#![forbid(unsafe_code)]

mod atomic;
mod backup;
mod chunker;
mod crypto;
mod engine;
mod identity;
mod key_envelope;
mod manifest;
mod pairing;
mod recovery;
mod replication;
mod restore;
mod storage;

use std::fs;
use std::path::{Path, PathBuf};

use covalent_protocol::{ContractError, ExportedDeviceSettings, RelativePath, ReplicaIntent};
use thiserror::Error;

pub use backup::{BackupOptions, BackupProgress, BackupResult, ExclusionRules, SymlinkPolicy};
pub use chunker::{ChunkingConfig, ContentDefinedChunker, DEFAULT_AVERAGE_CHUNK_SIZE};
pub use crypto::{BackupKey, EncryptedChunk};
pub use engine::{
    Engine, EngineOptions, JobControl, JobState, NodeConfig, RecoveredBackup,
    RememberedBackupState, RosterCursor, SnapshotAvailabilityReport,
};
pub use identity::{DeviceIdentity, PublicIdentity};
pub use key_envelope::{KeyEncryptionKey, SecretBinding, WrappedSecret};
pub use manifest::{SignedRosterBuilder, decrypt_manifest, encrypt_manifest, verify_roster};
pub use pairing::{
    PairingConfirmation, PairingManager, PairingSession, PairingSide, ShortAuthenticationString,
};
pub use recovery::{
    RecoveryCapsule, RecoveryKit, RecoveryProviderDirectoryEntry, RecoveryUnlockKey,
};
pub use replication::{
    ChunkProvider, ProviderFailure, ProviderHealth, ReplicationReport, ReplicationScheduler,
    StoreProvider,
};
pub use restore::{
    PreviewAction, RestoreOptions, RestorePlan, RestorePreviewEntry, RestoreReport,
    canonical_restore_actions_digest, canonical_target_inventory_digest,
};
pub use storage::{
    ChunkStore, GarbageCollectionReport, IntegrityReport, ProviderCapacity, ProviderQuotaPolicy,
    RecoveryCapsuleDescriptor, StoredSnapshot,
};

/// A canonical directory explicitly authorized as one restore boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedRoot {
    canonical: PathBuf,
}

impl AuthorizedRoot {
    /// Opens an existing, non-symlink directory as a restore boundary.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let metadata = fs::symlink_metadata(path).map_err(|source| CoreError::Io {
            operation: "inspect authorized root",
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(CoreError::InvalidAuthorizedRoot(path.to_path_buf()));
        }
        let canonical = fs::canonicalize(path).map_err(|source| CoreError::Io {
            operation: "canonicalize authorized root",
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self { canonical })
    }

    /// Returns the immutable canonical root selected by the user.
    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical
    }

    /// Resolves a validated protocol path while rejecting every existing symlink.
    pub fn resolve(&self, relative: &RelativePath) -> Result<PathBuf, CoreError> {
        let mut destination = self.canonical.clone();
        let mut missing_ancestor = false;
        let components: Vec<_> = relative.components().collect();

        for (index, component) in components.iter().enumerate() {
            destination.push(component);
            if missing_ancestor {
                continue;
            }
            match fs::symlink_metadata(&destination) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(CoreError::SymlinkTraversal(destination));
                    }
                    let is_final = index + 1 == components.len();
                    if !is_final && !metadata.is_dir() {
                        return Err(CoreError::NonDirectoryAncestor(destination));
                    }
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    missing_ancestor = true;
                }
                Err(source) => {
                    return Err(CoreError::Io {
                        operation: "inspect restore path",
                        path: destination,
                        source,
                    });
                }
            }
        }

        if !destination.starts_with(&self.canonical) {
            return Err(CoreError::EscapedAuthorizedRoot(destination));
        }
        Ok(destination)
    }
}

/// Parses a safe settings import. Identity keys are absent from the schema.
pub fn import_settings(bytes: &[u8]) -> Result<ExportedDeviceSettings, CoreError> {
    if bytes.len() > 1_048_576 {
        return Err(CoreError::SettingsTooLarge);
    }
    let settings: ExportedDeviceSettings = serde_json::from_slice(bytes)?;
    settings.validate()?;
    Ok(settings)
}

/// Serializes only the explicitly export-safe settings contract.
pub fn export_settings(settings: &ExportedDeviceSettings) -> Result<Vec<u8>, CoreError> {
    settings.validate()?;
    Ok(serde_json::to_vec_pretty(settings)?)
}

/// Computes a deterministic BLAKE3 digest for integrity verification.
#[must_use]
pub fn digest_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// A backup plan whose provider set can only originate from explicit intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupPlan {
    replica_intent: ReplicaIntent,
}

impl BackupPlan {
    /// Creates a plan from the exact user-selected provider set.
    #[must_use]
    pub const fn from_explicit_intent(replica_intent: ReplicaIntent) -> Self {
        Self { replica_intent }
    }

    /// Returns the immutable explicit intent. There is no auto-placement API.
    #[must_use]
    pub const fn replica_intent(&self) -> &ReplicaIntent {
        &self.replica_intent
    }
}

/// Shared engine error with stable safety-relevant categories.
#[derive(Debug, Error)]
pub enum CoreError {
    /// The authorized root does not exist as a real directory.
    #[error("authorized root is not a real directory: {0}")]
    InvalidAuthorizedRoot(PathBuf),
    /// An existing symlink could redirect a restore.
    #[error("restore path crosses a symlink: {0}")]
    SymlinkTraversal(PathBuf),
    /// An existing non-directory appears before the final path component.
    #[error("restore path has a non-directory ancestor: {0}")]
    NonDirectoryAncestor(PathBuf),
    /// A defense-in-depth root containment check failed.
    #[error("restore path escaped the authorized root: {0}")]
    EscapedAuthorizedRoot(PathBuf),
    /// Settings imports are bounded before decoding.
    #[error("settings import exceeds 1 MiB")]
    SettingsTooLarge,
    /// A key, nonce, signature, or authenticated record is invalid.
    #[error("cryptographic authentication failed")]
    AuthenticationFailed,
    /// Key material had an invalid encoded length.
    #[error("invalid cryptographic key material")]
    InvalidKeyMaterial,
    /// A record used an unsupported algorithm or version.
    #[error("unsupported cryptographic suite: {0}")]
    UnsupportedCipherSuite(String),
    /// A persisted state file has invalid invariants.
    #[error("invalid persisted state: {0}")]
    InvalidState(String),
    /// A requested encrypted chunk is absent.
    #[error("chunk is missing: {0}")]
    MissingChunk(String),
    /// A provider returned corrupt or mismatched content.
    #[error("chunk verification failed: {0}")]
    CorruptChunk(String),
    /// An opaque locator was malformed.
    #[error("invalid opaque chunk locator")]
    InvalidLocator,
    /// The source changed while it was being read.
    #[error("source changed during backup: {0}")]
    SourceChanged(PathBuf),
    /// Source content cannot be represented under the selected policy.
    #[error("unsupported source entry: {0}")]
    UnsupportedSourceEntry(PathBuf),
    /// A permission failure was reported without silently skipping content.
    #[error("source permission denied: {0}")]
    SourcePermissionDenied(PathBuf),
    /// A pairing invitation is invalid, expired, or already consumed.
    #[error("pairing invitation is unavailable")]
    InvitationUnavailable,
    /// Pairing still needs explicit user confirmation.
    #[error("pairing requires explicit confirmation")]
    PairingNotConfirmed,
    /// Settings replacement needs an explicit local confirmation.
    #[error("settings import requires explicit confirmation")]
    SettingsImportNotConfirmed,
    /// The remote identity did not match the confirmed invitation.
    #[error("paired identity mismatch")]
    IdentityMismatch,
    /// A revoked peer attempted an operation.
    #[error("peer is revoked")]
    PeerRevoked,
    /// A provider was not present in the exact explicit replica intent.
    #[error("provider was not explicitly selected")]
    UnselectedProvider,
    /// No mutually supported non-downgraded protocol exists.
    #[error("protocol negotiation failed")]
    ProtocolNegotiationFailed,
    /// An operation exceeded an explicit resource limit.
    #[error("resource limit exceeded: {0}")]
    ResourceLimit(&'static str),
    /// A resumable job is currently paused.
    #[error("job paused")]
    Paused,
    /// A job was explicitly cancelled.
    #[error("job cancelled")]
    Cancelled,
    /// Restore execution did not match the immutable preview.
    #[error("restore plan changed after preview")]
    RestorePlanMismatch,
    /// Restore conflict policy prohibited a write.
    #[error("restore conflict at {0}")]
    RestoreConflict(PathBuf),
    /// No intact authorized provider could supply an object.
    #[error("no intact authorized provider could supply {0}")]
    ProvidersExhausted(String),
    /// A synchronization primitive was poisoned.
    #[error("engine synchronization state is unavailable")]
    Synchronization,
    /// Another process already owns this durable engine state directory.
    #[error("engine state is already open by another process")]
    StateLocked,
    /// A filesystem operation failed.
    #[error("could not {operation} at {path}: {source}")]
    Io {
        /// Stable operation description.
        operation: &'static str,
        /// Affected local path.
        path: PathBuf,
        /// Original operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// A protocol contract was invalid.
    #[error(transparent)]
    Contract(#[from] ContractError),
    /// JSON did not match a strict persisted contract.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use covalent_protocol::{ExportedDeviceSettings, RelativePath};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn restore_stays_beneath_authorized_root() {
        let directory = tempdir().expect("temporary root");
        fs::create_dir(directory.path().join("nested")).expect("nested directory");
        let root = AuthorizedRoot::open(directory.path()).expect("authorized root");
        let relative = RelativePath::new("nested/file.txt").expect("relative path");
        let destination = root.resolve(&relative).expect("safe destination");
        assert_eq!(destination, root.canonical_path().join("nested/file.txt"));
        assert!(destination.starts_with(root.canonical_path()));
    }

    #[cfg(unix)]
    #[test]
    fn restore_rejects_intermediate_and_final_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary root");
        let outside = tempdir().expect("outside directory");
        symlink(outside.path(), directory.path().join("redirect")).expect("directory symlink");
        symlink(
            outside.path().join("file"),
            directory.path().join("final-link"),
        )
        .expect("file symlink");
        let root = AuthorizedRoot::open(directory.path()).expect("authorized root");

        for relative in ["redirect/stolen.txt", "final-link"] {
            let error = root
                .resolve(&RelativePath::new(relative).expect("relative path"))
                .expect_err("symlink must fail");
            assert!(matches!(error, CoreError::SymlinkTraversal(_)));
        }
    }

    #[test]
    fn normal_settings_export_never_contains_identity_material() {
        let settings =
            ExportedDeviceSettings::new("Home Mac", false, Vec::new()).expect("valid settings");
        let json = export_settings(&settings).expect("settings export");
        let text = String::from_utf8(json.clone()).expect("utf8 JSON");
        assert!(!text.to_ascii_lowercase().contains("private"));
        assert!(!text.to_ascii_lowercase().contains("identitykey"));
        assert_eq!(import_settings(&json).expect("settings import"), settings);
    }

    #[test]
    fn settings_import_is_bounded_before_parsing() {
        assert!(matches!(
            import_settings(&vec![b' '; 1_048_577]),
            Err(CoreError::SettingsTooLarge)
        ));
    }
}
