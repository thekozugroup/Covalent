//! Native/container host provisioning for the maintained folder engine.

use std::fs::{DirBuilder, File};
use std::net::SocketAddr;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use covalent_core::sync::state_dir::PrivateStateInventoryName;
use covalent_core::{Engine, KeyProtector};

use super::{
    EngineInstallation, FolderSharingJournal, FolderSyncAccessRecovery, FolderSyncService,
    VerifiedEngineExecutable,
};

/// Host-supplied packaged executables and a reachable direct sync endpoint.
/// Executables must already have been checked against the host's pinned
/// manifest. This object contains no user-entered engine ID or API credential.
pub struct FolderSyncRuntimeConfig {
    pub guardian: VerifiedEngineExecutable,
    pub worker: VerifiedEngineExecutable,
    pub runtime_parent: PathBuf,
    pub listener: SocketAddr,
    /// A concrete explicit override, or None to use the runtime's selected
    /// QUIC advertised IP with this listener's separate TCP port.
    pub advertised_address: Option<SocketAddr>,
}

/// Runtime availability remains independent of backup and owner recovery.
#[derive(Clone, Default)]
pub(crate) enum FolderSyncRuntimeState {
    #[default]
    NotPackaged,
    NeedsAttention,
    FolderAccessUnavailable(Option<Arc<FolderSyncAccessRecovery>>),
    Ready(Arc<FolderSyncService>),
}

impl FolderSyncRuntimeConfig {
    /// Open only an existing installation and authenticated share journal after
    /// native folder capabilities were lost. Missing state remains an empty
    /// unavailable state; damaged incumbent state fails closed.
    pub(crate) fn prepare_access_recovery(
        data_directory: &Path,
        engine: Arc<Engine>,
        protector: Arc<dyn KeyProtector>,
    ) -> FolderSyncRuntimeState {
        let root = data_directory.join("folder-sync");
        match std::fs::symlink_metadata(&root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return FolderSyncRuntimeState::FolderAccessUnavailable(None);
            }
            Err(_) => return FolderSyncRuntimeState::NeedsAttention,
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return FolderSyncRuntimeState::NeedsAttention,
        }
        let installation = match EngineInstallation::open(&root, protector.as_ref()) {
            Ok(installation) => Arc::new(installation),
            Err(_) => return FolderSyncRuntimeState::NeedsAttention,
        };
        match std::fs::symlink_metadata(root.join("folder-sharing.v1")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let inventory =
                    installation
                        .state_directory()
                        .inventory(installation.state_lock(), 8, 1024);
                return match inventory {
                    Ok(inventory)
                        if inventory.entries().iter().all(|entry| match entry.name() {
                            PrivateStateInventoryName::WriterLock => true,
                            PrivateStateInventoryName::State(key) => {
                                key.as_str() == "engine-identity.v1"
                            }
                        }) =>
                    {
                        FolderSyncRuntimeState::FolderAccessUnavailable(None)
                    }
                    _ => FolderSyncRuntimeState::NeedsAttention,
                };
            }
            Err(_) => return FolderSyncRuntimeState::NeedsAttention,
            Ok(_) => {}
        }
        let journal = match FolderSharingJournal::open(engine, Arc::clone(&installation), protector)
        {
            Ok(journal) => journal,
            Err(_) => return FolderSyncRuntimeState::NeedsAttention,
        };
        match FolderSyncAccessRecovery::new(journal, installation) {
            Ok(recovery) => {
                FolderSyncRuntimeState::FolderAccessUnavailable(Some(Arc::new(recovery)))
            }
            Err(_) => FolderSyncRuntimeState::NeedsAttention,
        }
    }

    pub(crate) async fn prepare(
        self,
        data_directory: &Path,
        engine: Arc<Engine>,
        protector: Arc<dyn KeyProtector>,
        advertised_peer_address: Option<SocketAddr>,
    ) -> FolderSyncRuntimeState {
        let Some(advertised) = self.advertised_address.or_else(|| {
            advertised_peer_address.map(|peer| SocketAddr::new(peer.ip(), self.listener.port()))
        }) else {
            return FolderSyncRuntimeState::NeedsAttention;
        };
        match self.prepare_inner(data_directory, engine, protector, advertised) {
            Ok(service) => FolderSyncRuntimeState::Ready(Arc::new(service)),
            Err(()) => FolderSyncRuntimeState::NeedsAttention,
        }
    }

    fn prepare_inner(
        self,
        data_directory: &Path,
        engine: Arc<Engine>,
        protector: Arc<dyn KeyProtector>,
        advertised_address: SocketAddr,
    ) -> Result<FolderSyncService, ()> {
        // Validate before creating state. Existing historical invitations keep
        // their signed bytes; the journal separately advances the mutable
        // current local route peers can authenticate through a live proof.
        if self.listener.port() == 0
            || self.listener.port() != advertised_address.port()
            || self.listener.is_ipv4() != advertised_address.is_ipv4()
            || (!self.listener.ip().is_unspecified()
                && self.listener.ip() != advertised_address.ip())
        {
            return Err(());
        }
        let root = data_directory.join("folder-sync");
        let created = match DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => {
                File::open(data_directory)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| ())?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err(()),
        };
        let installation = Arc::new(
            if created {
                EngineInstallation::create(&root, protector.as_ref())
            } else {
                EngineInstallation::open(&root, protector.as_ref())
            }
            .map_err(|_| ())?,
        );
        let journal = if created {
            FolderSharingJournal::create(
                Arc::clone(&engine),
                Arc::clone(&installation),
                protector,
                self.listener,
                advertised_address,
            )
        } else {
            FolderSharingJournal::open_at_route(
                Arc::clone(&engine),
                Arc::clone(&installation),
                protector,
                self.listener,
                advertised_address,
            )
        }
        .map_err(|_| ())?;
        FolderSyncService::new(
            journal,
            installation,
            engine,
            self.guardian,
            self.worker,
            self.runtime_parent,
        )
        .map_err(|_| ())
    }
}
