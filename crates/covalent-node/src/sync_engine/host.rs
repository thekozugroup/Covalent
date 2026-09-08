//! Native/container host provisioning for the maintained folder engine.

use std::fs::{DirBuilder, File};
use std::net::SocketAddr;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use covalent_core::{Engine, KeyProtector};

use super::{
    EngineInstallation, FolderSharingJournal, FolderSyncService, VerifiedEngineExecutable,
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
    FolderAccessUnavailable,
    Ready(Arc<FolderSyncService>),
}

impl FolderSyncRuntimeConfig {
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
            Ok(service) => {
                // A launch failure is retained in the service for a visible,
                // explicit retry; no failure rolls back durable consent.
                let _ = service.start().await;
                FolderSyncRuntimeState::Ready(Arc::new(service))
            }
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
        // Validate before creating state. An existing signed endpoint cannot
        // silently move: its peers retain the old signed address.
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
            FolderSharingJournal::open(Arc::clone(&engine), Arc::clone(&installation), protector)
        }
        .map_err(|_| ())?;
        if journal.binding().direct_address != advertised_address.to_string()
            || journal.listener() != self.listener
        {
            return Err(());
        }
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
