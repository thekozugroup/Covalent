//! Durable, root-bound engine identity and exclusive installation ownership.
//!
//! Opening and creation are deliberately separate. Missing or damaged identity
//! state must never silently create a second identity against an existing DB.

use std::fmt;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use covalent_core::sync::state_dir::{
    PrivateStateDir, PrivateStateInventoryName, PrivateStateLock, StateKey,
};
use covalent_core::{KeyProtector, WrappedSecret, state_secret_context};
use zeroize::Zeroizing;

use super::EngineIdentity;
use super::config::EngineDeviceId;

const IDENTITY_RECORD: &str = "engine-identity.v1";
const MAX_RECORD_BYTES: u64 = 128 * 1024;

/// Fixed errors omit local paths, identity material, and platform error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineInstallationError {
    /// The canonical private installation root or its lock is unavailable.
    Unavailable,
    /// Creation was requested against a root containing prior state.
    NotFresh,
    /// A stored identity is missing, malformed, or cannot be authenticated.
    InvalidIdentity,
    /// An exclusive durable creation did not report success. Reopen to inspect.
    PersistenceUncertain,
}

impl fmt::Display for EngineInstallationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "folder sync installation is unavailable",
            Self::NotFresh => "folder sync installation already contains state",
            Self::InvalidIdentity => "folder sync installation identity cannot be unlocked",
            Self::PersistenceUncertain => "folder sync installation creation needs recovery",
        })
    }
}

impl std::error::Error for EngineInstallationError {}

/// One exclusive engine installation. The controller and process reaper share
/// this through an `Arc`; its lock must remain held until the worker is reaped.
/// The protected identity is excluded from owner-loss backup/recovery exports.
pub struct EngineInstallation {
    root: PathBuf,
    directory: PrivateStateDir,
    lock: PrivateStateLock,
    directory_identity: (u64, u64),
    identity: EngineIdentity,
    device_id: EngineDeviceId,
}

impl fmt::Debug for EngineInstallation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EngineInstallation([PRIVATE])")
    }
}

impl EngineInstallation {
    /// Create in an existing fresh, canonical mode-0700 directory. The caller
    /// owns creation and durability of that root's own directory entry.
    /// Failure after exclusive record creation leaves the incumbent untouched.
    pub fn create(
        root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<Self, EngineInstallationError> {
        let (directory, directory_identity) = anchor(root)?;
        let lock = directory
            .try_lock()
            .map_err(|_| EngineInstallationError::Unavailable)?;
        let inventory = directory
            .inventory(&lock, 8, 1024)
            .map_err(|_| EngineInstallationError::NotFresh)?;
        if inventory
            .entries()
            .iter()
            .any(|entry| !matches!(entry.name(), PrivateStateInventoryName::WriterLock))
        {
            return Err(EngineInstallationError::NotFresh);
        }
        let context = state_secret_context(root, IDENTITY_RECORD);
        let record = EngineIdentity::generate()
            .and_then(|identity| identity.into_protected(protector, &context))
            .map_err(|_| EngineInstallationError::InvalidIdentity)?;
        let bytes = Zeroizing::new(
            serde_json::to_vec(&record).map_err(|_| EngineInstallationError::InvalidIdentity)?,
        );
        directory
            .create_new_file(&lock, &record_key()?, &bytes, MAX_RECORD_BYTES)
            .map_err(|_| EngineInstallationError::PersistenceUncertain)?;
        Self::finish(root, directory, lock, directory_identity, protector)
    }

    /// Reopen the exact root and existing lock. Missing, copied, corrupt, or
    /// wrong-key state fails without creating files or replacing the identity.
    pub fn open(
        root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<Self, EngineInstallationError> {
        let (directory, directory_identity) = anchor(root)?;
        let lock = directory
            .try_lock_existing()
            .map_err(|_| EngineInstallationError::Unavailable)?;
        Self::finish(root, directory, lock, directory_identity, protector)
    }

    fn finish(
        root: &Path,
        directory: PrivateStateDir,
        lock: PrivateStateLock,
        directory_identity: (u64, u64),
        protector: &dyn KeyProtector,
    ) -> Result<Self, EngineInstallationError> {
        let bytes = Zeroizing::new(
            directory
                .open_file(&record_key()?, MAX_RECORD_BYTES)
                .and_then(|file| file.read_all())
                .map_err(|_| EngineInstallationError::InvalidIdentity)?,
        );
        let record: WrappedSecret =
            serde_json::from_slice(&bytes).map_err(|_| EngineInstallationError::InvalidIdentity)?;
        let context = state_secret_context(root, IDENTITY_RECORD);
        let identity = EngineIdentity::from_protected(&record, protector, &context)
            .map_err(|_| EngineInstallationError::InvalidIdentity)?;
        let device_id = EngineDeviceId::from_certificate_der(identity.certificate_der())
            .map_err(|_| EngineInstallationError::InvalidIdentity)?;
        let installation = Self {
            root: root.to_path_buf(),
            directory,
            lock,
            directory_identity,
            identity,
            device_id,
        };
        installation.revalidate()?;
        Ok(installation)
    }

    /// The public engine identity, stable across ordinary restarts.
    pub fn device_id(&self) -> &EngineDeviceId {
        &self.device_id
    }

    /// Revalidate the canonical root entry and held cooperative writer lock.
    /// Ancestor protection is supplied by the native app/container state root;
    /// another process acting as this same UID is outside this boundary.
    pub fn revalidate(&self) -> Result<(), EngineInstallationError> {
        let (_, identity) = anchor(&self.root)?;
        if identity != self.directory_identity {
            return Err(EngineInstallationError::Unavailable);
        }
        self.lock
            .validate()
            .map_err(|_| EngineInstallationError::Unavailable)
    }

    /// Durably ensure the private database directory after identity creation.
    /// No worker is launched here and this never creates identity defaults.
    pub fn database_directory(&self) -> Result<PathBuf, EngineInstallationError> {
        self.revalidate()?;
        let key = StateKey::new("database").map_err(|_| EngineInstallationError::Unavailable)?;
        self.directory
            .open_or_create_child(&key)
            .map_err(|_| EngineInstallationError::Unavailable)?;
        self.revalidate()?;
        Ok(self.root.join(key.as_str()))
    }

    pub(super) fn identity(&self) -> &EngineIdentity {
        &self.identity
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }
}

fn anchor(root: &Path) -> Result<(PrivateStateDir, (u64, u64)), EngineInstallationError> {
    if !root.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || std::fs::canonicalize(root).map_err(|_| EngineInstallationError::Unavailable)? != root
    {
        return Err(EngineInstallationError::Unavailable);
    }
    let directory =
        PrivateStateDir::open_root(root).map_err(|_| EngineInstallationError::Unavailable)?;
    let metadata =
        std::fs::symlink_metadata(root).map_err(|_| EngineInstallationError::Unavailable)?;
    if !metadata.is_dir() {
        return Err(EngineInstallationError::Unavailable);
    }
    Ok((directory, (metadata.dev(), metadata.ino())))
}

fn record_key() -> Result<StateKey, EngineInstallationError> {
    StateKey::new(IDENTITY_RECORD).map_err(|_| EngineInstallationError::InvalidIdentity)
}

#[cfg(test)]
#[path = "installation_tests.rs"]
mod tests;
