//! One bounded engine session beneath a caller-owned durable installation.
//!
//! Membership authorization and durable desired-state reconciliation precede
//! this module. It cannot accept a remote invitation or grant folder access.

use std::fmt;
use std::fs::File;
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use covalent_core::sync::state_dir::{PrivateStateDir, StateKey};
use rand_core::{OsRng, RngCore as _};
use serde_json::Value;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use super::config::{
    DesiredEngineConfig, EngineApiKey, EngineFolderConfig, EngineGuiEndpoint, EnginePeerConfig,
};
use super::installation::EngineInstallation;
use super::{
    EngineApiClient, EngineApiError, EngineEndpoint, OwnedEngineWorker, StopOutcome,
    VerifiedEngineExecutable,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const INITIAL_SCAN_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const PINNED_ENGINE_VERSION: &str = "v2.1.3";

/// Already-authorized desired state for one engine session. The enclosing
/// controller must persist and reconcile it with Covalent's current peer grants
/// before launch; these values are never a substitute for pairing authority.
#[derive(Clone, Eq, PartialEq)]
pub struct EngineSessionSettings {
    /// Local device display name.
    pub device_name: String,
    /// Explicit direct sync listener; absent for a network-inert session.
    pub listener: Option<SocketAddr>,
    /// Explicit authenticated peer bindings.
    pub peers: Vec<EnginePeerConfig>,
    /// Explicit user-selected roots and accepted memberships.
    pub folders: Vec<EngineFolderConfig>,
}

/// Redacted session failure; details are kept inside the local engine boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineSessionError {
    /// Identity, paths, folder capabilities, or desired inputs are invalid.
    InvalidConfiguration,
    /// Private runtime material could not be created durably.
    RuntimeUnavailable,
    /// A pinned guardian/worker could not be launched.
    LaunchFailed,
    /// The expected worker did not become ready before its whole deadline.
    StartupTimeout,
    /// Worker identity, version, or effective settings differ from desired state.
    StartupMismatch,
    /// The running worker's private API is unavailable.
    EngineUnavailable,
}

impl fmt::Display for EngineSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "folder sync configuration is invalid",
            Self::RuntimeUnavailable => "folder sync runtime storage is unavailable",
            Self::LaunchFailed => "folder sync worker could not start",
            Self::StartupTimeout => "folder sync worker did not become ready",
            Self::StartupMismatch => "folder sync worker failed startup verification",
            Self::EngineUnavailable => "folder sync worker is unavailable",
        })
    }
}

impl std::error::Error for EngineSessionError {}

/// A verified owned session. It starts with every network path disabled while
/// its authorized folders are scanned and becomes active only after explicit
/// promotion. Dropping it closes the guardian lifeline; the dedicated reaper
/// retains runtime files, installation lock and selected-root descriptors
/// until the actual child exits. A stop timeout is not completion.
pub struct ManagedEngineSession {
    worker: OwnedEngineWorker,
    client: Arc<EngineApiClient>,
    configuration: DesiredEngineConfig,
    installation: Arc<EngineInstallation>,
    roots: Arc<Vec<FolderRootLease>>,
    reset_gate: bool,
}

/// One controller-owned, cancellable initial scan. Dropping this value aborts
/// the active private API request; the session owner separately closes and
/// reaps the worker before releasing its filesystem capabilities.
pub(super) struct InitialScanTask {
    task: Option<JoinHandle<Result<(), EngineSessionError>>>,
}

impl InitialScanTask {
    pub(super) fn new(task: JoinHandle<Result<(), EngineSessionError>>) -> Self {
        Self { task: Some(task) }
    }

    pub(super) fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub(super) fn abort(&self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }

    pub(super) async fn finish(&mut self) -> Result<(), EngineSessionError> {
        let Some(task) = self.task.take() else {
            return Err(EngineSessionError::EngineUnavailable);
        };
        task.await
            .map_err(|_| EngineSessionError::EngineUnavailable)?
    }
}

impl Drop for InitialScanTask {
    fn drop(&mut self) {
        self.abort();
    }
}

impl fmt::Debug for ManagedEngineSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManagedEngineSession([PRIVATE])")
    }
}

// Field ownership, not an async cleanup task, ties these capabilities to the
// native child wait. TempDir cannot be dropped by cancellation during startup.
struct WorkerResources {
    _runtime: tempfile::TempDir,
    _installation: Arc<EngineInstallation>,
    _roots: Arc<Vec<FolderRootLease>>,
    _worker_lease: super::installation::EngineWorkerLease,
}

struct FolderRootLease {
    path: PathBuf,
    file: File,
    identity: (u64, u64),
}

struct PromotionGuard<'a> {
    worker: &'a mut OwnedEngineWorker,
    armed: bool,
}

impl PromotionGuard<'_> {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PromotionGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.worker.close_lifeline();
        }
    }
}

#[cfg(target_os = "macos")]
struct EphemeralGuiTls {
    address: SocketAddr,
    reservation: std::net::TcpListener,
    certificate_der: Vec<u8>,
    certificate_pem: String,
    private_key_pem: Zeroizing<String>,
}

#[cfg(target_os = "macos")]
impl EphemeralGuiTls {
    fn prepare() -> Result<Self, EngineSessionError> {
        let failed = || EngineSessionError::RuntimeUnavailable;
        let reservation = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|_| failed())?;
        let address = reservation.local_addr().map_err(|_| failed())?;
        let key = rcgen::KeyPair::generate().map_err(|_| failed())?;
        let mut params =
            rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
                .map_err(|_| failed())?;
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now
            .checked_sub(time::Duration::days(1))
            .ok_or_else(failed)?;
        // Validity must not strand a continuously running home server. Trust
        // is restricted to this fresh session's exact leaf, not its issuer.
        params.not_after = now
            .checked_add(time::Duration::days(20 * 365))
            .ok_or_else(failed)?;
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params.self_signed(&key).map_err(|_| failed())?;
        Ok(Self {
            address,
            reservation,
            certificate_der: certificate.der().to_vec(),
            certificate_pem: certificate.pem(),
            private_key_pem: Zeroizing::new(key.serialize_pem()),
        })
    }
}

impl ManagedEngineSession {
    /// Create fresh per-run private configuration, launch the exact pinned
    /// worker, and verify its version, identity and effective scan-gate state.
    /// `runtime_parent` is the native host's authorized temporary location; a
    /// short path is required on hosts using Unix control. macOS uses pinned
    /// loopback TLS within its app sandbox. This location is separate from the
    /// durable database root and contains no inherited engine configuration.
    pub async fn start(
        installation: Arc<EngineInstallation>,
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        runtime_parent: &Path,
        settings: EngineSessionSettings,
    ) -> Result<Self, EngineSessionError> {
        Self::start_with_gate(
            installation,
            guardian,
            engine,
            runtime_parent,
            settings,
            false,
        )
        .await
    }

    pub(super) async fn start_reset_gate(
        installation: Arc<EngineInstallation>,
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        runtime_parent: &Path,
        settings: EngineSessionSettings,
    ) -> Result<Self, EngineSessionError> {
        Self::start_with_gate(
            installation,
            guardian,
            engine,
            runtime_parent,
            settings,
            true,
        )
        .await
    }

    async fn start_with_gate(
        installation: Arc<EngineInstallation>,
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        runtime_parent: &Path,
        settings: EngineSessionSettings,
        reset_gate: bool,
    ) -> Result<Self, EngineSessionError> {
        let worker_lease = installation
            .claim_worker()
            .map_err(|_| EngineSessionError::LaunchFailed)?;
        installation
            .revalidate()
            .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        let runtime = tempfile::Builder::new()
            .prefix("cv-engine-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(runtime_parent)
            .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let config_dir = std::fs::canonicalize(runtime.path())
            .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let private = PrivateStateDir::open_root(&config_dir)
            .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let lock = private
            .try_lock()
            .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let mut random = Zeroizing::new([0_u8; 32]);
        OsRng
            .try_fill_bytes(random.as_mut())
            .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let mut api_key = Zeroizing::new(String::with_capacity(64));
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in random.iter() {
            api_key.push(char::from(HEX[usize::from(byte >> 4)]));
            api_key.push(char::from(HEX[usize::from(byte & 15)]));
        }
        #[cfg(target_os = "macos")]
        let tls = EphemeralGuiTls::prepare()?;
        #[cfg(target_os = "macos")]
        let control = EngineGuiEndpoint::LoopbackTls {
            address: tls.address,
            private_root: config_dir.clone(),
        };
        #[cfg(not(target_os = "macos"))]
        let control = EngineGuiEndpoint::Unix(config_dir.join("api.sock"));
        let configuration = DesiredEngineConfig::with_control(
            installation.device_id().clone(),
            &settings.device_name,
            control,
            EngineApiKey::parse(api_key).map_err(|_| EngineSessionError::InvalidConfiguration)?,
            settings.listener,
            settings.peers,
            settings.folders,
        )
        .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        let roots = Arc::new(
            configuration
                .folders()
                .iter()
                .map(|folder| admit_root(folder.root(), installation.root(), &config_dir))
                .collect::<Result<Vec<_>, _>>()?,
        );
        let xml = if reset_gate {
            configuration.render_reset_gate_xml()
        } else {
            configuration.render_scan_gate_xml()
        }
        .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        let certificate = installation.identity().certificate_pem();
        for (name, bytes, maximum) in [
            ("config.xml", xml.as_bytes(), 4 * 1024 * 1024),
            ("cert.pem", certificate.as_bytes(), 24 * 1024),
            (
                "key.pem",
                installation.identity().private_key_pem().as_bytes(),
                4096,
            ),
        ] {
            let key = StateKey::new(name).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
            private
                .create_new_file(&lock, &key, bytes, maximum)
                .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        }
        #[cfg(target_os = "macos")]
        for (name, bytes) in [
            ("https-cert.pem", tls.certificate_pem.as_bytes()),
            ("https-key.pem", tls.private_key_pem.as_bytes()),
        ] {
            let key = StateKey::new(name).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
            private
                .create_new_file(&lock, &key, bytes, 24 * 1024)
                .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        }
        let database = installation
            .database_directory()
            .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        #[cfg(target_os = "macos")]
        let client = EngineApiClient::new_loopback_tls(
            tls.address,
            configuration.api_key_copy(),
            tls.certificate_der,
        )
        .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        #[cfg(not(target_os = "macos"))]
        let client =
            EngineApiClient::new(config_dir.join("api.sock"), configuration.api_key_copy())
                .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        let resources = WorkerResources {
            _runtime: runtime,
            _installation: Arc::clone(&installation),
            _roots: Arc::clone(&roots),
            _worker_lease: worker_lease,
        };
        // The worker cannot inherit a pre-bound GUI descriptor. Release the
        // reserved port only immediately before launch. A port race can deny
        // this startup, but certificate verification precedes every request,
        // so an unrelated listener never receives the session API key.
        #[cfg(target_os = "macos")]
        drop(tls.reservation);
        let worker =
            OwnedEngineWorker::launch(guardian, engine, config_dir, database, Box::new(resources))
                .map_err(|_| EngineSessionError::LaunchFailed)?;
        let mut session = Self {
            worker,
            client: Arc::new(client),
            configuration,
            installation,
            roots,
            reset_gate,
        };
        match tokio::time::timeout(STARTUP_TIMEOUT, session.verify_startup()).await {
            Ok(Ok(())) => Ok(session),
            result => {
                // Close before awaiting so cancellation cannot leave a running
                // worker. The reaper retains all leases through actual exit.
                session.worker.close_lifeline();
                let _ = session.worker.stop().await;
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => EngineSessionError::StartupTimeout,
                })
            }
        }
    }

    async fn verify_startup(&mut self) -> Result<(), EngineSessionError> {
        let version = loop {
            self.revalidate_roots()?;
            if self
                .worker
                .try_status()
                .map_err(|_| EngineSessionError::StartupMismatch)?
                .is_some()
            {
                return Err(EngineSessionError::StartupMismatch);
            }
            match self
                .client
                .json::<Value>(EngineEndpoint::SystemVersion)
                .await
            {
                Ok(version) => break version,
                Err(EngineApiError::Unavailable | EngineApiError::Timeout) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(_) => return Err(EngineSessionError::StartupMismatch),
            }
        };
        if version.get("version").and_then(Value::as_str) != Some(PINNED_ENGINE_VERSION) {
            return Err(EngineSessionError::StartupMismatch);
        }
        let status: Value = self
            .client
            .json(EngineEndpoint::SystemStatus)
            .await
            .map_err(|_| EngineSessionError::StartupMismatch)?;
        if status.get("myID").and_then(Value::as_str) != Some(self.configuration.own_id().as_str())
        {
            return Err(EngineSessionError::StartupMismatch);
        }
        let effective: Value = self
            .client
            .json(EngineEndpoint::Configuration)
            .await
            .map_err(|_| EngineSessionError::StartupMismatch)?;
        let verified = if self.reset_gate {
            self.configuration.verify_reset_gate_effective(&effective)
        } else {
            self.configuration.verify_scan_gate_effective(&effective)
        };
        verified.map_err(|_| EngineSessionError::StartupMismatch)?;
        self.revalidate_roots()
    }

    /// Begin one positive full scan of every authorized root. The task owns no
    /// worker or filesystem lease, so its owner must retain this session and
    /// reap it if the task fails or is cancelled.
    pub(super) fn begin_initial_scan(&self) -> InitialScanTask {
        let client = Arc::clone(&self.client);
        let installation = Arc::clone(&self.installation);
        let roots = Arc::clone(&self.roots);
        let folders = self
            .configuration
            .folders()
            .iter()
            .map(|folder| folder.id())
            .collect::<Vec<_>>();
        InitialScanTask::new(tokio::spawn(async move {
            tokio::time::timeout(INITIAL_SCAN_TIMEOUT, async {
                revalidate_capabilities(&installation, &roots)?;
                for folder in &folders {
                    revalidate_capabilities(&installation, &roots)?;
                    // Pinned v2.1.3 `postDBScan` synchronously calls
                    // `ScanFolderSubdirs(folder, nil)`. The latter waits for
                    // the automatic initial scan, then runs this requested
                    // full scan through the folder runner before responding.
                    client
                        .scan_folder(*folder, INITIAL_SCAN_TIMEOUT)
                        .await
                        .map_err(|_| EngineSessionError::EngineUnavailable)?;
                    revalidate_capabilities(&installation, &roots)?;
                }
                // Earlier roots may become unhealthy while a later full scan
                // runs. Require one fresh all-folder observation at the end.
                let health = super::collect_folder_health(&client, &folders)
                    .await
                    .map_err(|_| EngineSessionError::EngineUnavailable)?;
                validate_initial_scan_health(&folders, &health)?;
                revalidate_capabilities(&installation, &roots)
            })
            .await
            .map_err(|_| EngineSessionError::EngineUnavailable)?
        }))
    }

    /// Ask pinned v2.1.3 to drop exactly one paused folder index, then require
    /// its documented restart exit. The caller retains a durable reset intent
    /// until this succeeds and the exact worker has been reaped.
    pub(super) async fn reset_folder_index(
        &mut self,
        folder: uuid::Uuid,
    ) -> Result<(), EngineSessionError> {
        self.revalidate_roots()?;
        if !self
            .configuration
            .folders()
            .iter()
            .any(|configured| configured.id() == folder)
        {
            return Err(EngineSessionError::InvalidConfiguration);
        }
        if !self.reset_gate {
            return Err(EngineSessionError::InvalidConfiguration);
        }
        self.client
            .command(EngineEndpoint::ResetFolderIndex(folder))
            .await
            .map_err(|_| EngineSessionError::EngineUnavailable)?;
        let status = tokio::time::timeout(STARTUP_TIMEOUT, async {
            loop {
                if let Some(status) = self
                    .worker
                    .try_status()
                    .map_err(|_| EngineSessionError::EngineUnavailable)?
                {
                    return Ok(status);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| EngineSessionError::EngineUnavailable)??;
        if status.code() != Some(3) {
            return Err(EngineSessionError::EngineUnavailable);
        }
        self.revalidate_roots()?;
        self.ensure_folder_marker(folder)?;
        self.revalidate_roots()
    }

    fn ensure_folder_marker(&self, folder: uuid::Uuid) -> Result<(), EngineSessionError> {
        let index = self
            .configuration
            .folders()
            .iter()
            .position(|configured| configured.id() == folder)
            .ok_or(EngineSessionError::InvalidConfiguration)?;
        let root = self
            .roots
            .get(index)
            .ok_or(EngineSessionError::InvalidConfiguration)?;
        match rustix::fs::mkdirat(
            &root.file,
            ".stfolder",
            rustix::fs::Mode::from_bits_truncate(0o700),
        ) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(_) => return Err(EngineSessionError::RuntimeUnavailable),
        }
        let marker = rustix::fs::openat(
            &root.file,
            ".stfolder",
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let before =
            rustix::fs::fstat(&marker).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        if rustix::fs::FileType::from_raw_mode(before.st_mode) != rustix::fs::FileType::Directory {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        rustix::fs::fsync(&marker).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        rustix::fs::fsync(&root.file).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let current = rustix::fs::statat(
            &root.file,
            ".stfolder",
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        let after =
            rustix::fs::fstat(&marker).map_err(|_| EngineSessionError::RuntimeUnavailable)?;
        if (current.st_dev, current.st_ino, current.st_mode)
            != (before.st_dev, before.st_ino, before.st_mode)
            || (after.st_dev, after.st_ino, after.st_mode)
                != (before.st_dev, before.st_ino, before.st_mode)
        {
            return Err(EngineSessionError::RuntimeUnavailable);
        }
        Ok(())
    }

    /// Promote a successfully scanned, still-network-inert session to its
    /// exact desired listener and peer state, then verify the resulting worker.
    pub(super) async fn promote_after_initial_scan(&mut self) -> Result<(), EngineSessionError> {
        self.revalidate_roots()?;
        let client = Arc::clone(&self.client);
        let installation = Arc::clone(&self.installation);
        let roots = Arc::clone(&self.roots);
        let configuration = &self.configuration;
        let mut guard = PromotionGuard {
            worker: &mut self.worker,
            armed: true,
        };
        let effective: Value = client
            .json(EngineEndpoint::Configuration)
            .await
            .map_err(|_| EngineSessionError::StartupMismatch)?;
        let promotion = configuration
            .promotion_payload(effective)
            .map_err(|_| EngineSessionError::StartupMismatch)?;
        revalidate_capabilities(&installation, &roots)?;
        client
            .replace_configuration(&promotion)
            .await
            .map_err(|_| EngineSessionError::StartupMismatch)?;
        let result = tokio::time::timeout(STARTUP_TIMEOUT, async {
            loop {
                revalidate_capabilities(&installation, &roots)?;
                if guard
                    .worker
                    .try_status()
                    .map_err(|_| EngineSessionError::StartupMismatch)?
                    .is_some()
                {
                    return Err(EngineSessionError::StartupMismatch);
                }
                let effective: Result<Value, _> = client.json(EngineEndpoint::Configuration).await;
                match effective {
                    Ok(effective) => {
                        configuration
                            .verify_effective(&effective)
                            .map_err(|_| EngineSessionError::StartupMismatch)?;
                        let status: Value = client
                            .json(EngineEndpoint::SystemStatus)
                            .await
                            .map_err(|_| EngineSessionError::StartupMismatch)?;
                        if status.get("myID").and_then(Value::as_str)
                            != Some(configuration.own_id().as_str())
                        {
                            return Err(EngineSessionError::StartupMismatch);
                        }
                        revalidate_capabilities(&installation, &roots)?;
                        return Ok(());
                    }
                    Err(EngineApiError::Unavailable | EngineApiError::Timeout) => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Err(_) => return Err(EngineSessionError::StartupMismatch),
                }
            }
        })
        .await
        .map_err(|_| EngineSessionError::StartupTimeout)?;
        result?;
        guard.disarm();
        Ok(())
    }

    /// Verify retained filesystem capabilities before consulting local status.
    /// The owning controller must also call this during its periodic health
    /// loop; stock workers do not consume these descriptors themselves.
    pub async fn status(&mut self) -> Result<Value, EngineSessionError> {
        if let Err(error) = self.revalidate_roots() {
            self.worker.close_lifeline();
            return Err(error);
        }
        self.client
            .json(EngineEndpoint::SystemStatus)
            .await
            .map_err(|_| EngineSessionError::EngineUnavailable)
    }

    /// Observe each authorized folder through the bounded private API. A
    /// successful observation is not a promise that an initial scan is complete
    /// or that every peer has received the same files.
    pub async fn folder_health(&mut self) -> Result<Vec<super::FolderHealth>, EngineSessionError> {
        if let Err(error) = self.revalidate_roots() {
            self.worker.close_lifeline();
            return Err(error);
        }
        let folders = self
            .configuration
            .folders()
            .iter()
            .map(|folder| folder.id())
            .collect::<Vec<_>>();
        let health = super::collect_folder_health(&self.client, &folders)
            .await
            .map_err(|_| EngineSessionError::EngineUnavailable)?;
        if let Err(error) = self.revalidate_roots() {
            self.worker.close_lifeline();
            return Err(error);
        }
        Ok(health)
    }

    /// Observe folder health and configured peer connections within one
    /// controller-owned polling cycle. Connection failure is kept separate so
    /// an ordinary unavailable/malformed connection response cannot turn a
    /// healthy folder worker into a stopped worker.
    pub async fn health_observation(
        &mut self,
    ) -> (
        Result<Vec<super::FolderHealth>, EngineSessionError>,
        Result<Vec<super::EnginePeerConnection>, EngineSessionError>,
    ) {
        if let Err(error) = self.revalidate_roots() {
            self.worker.close_lifeline();
            return (Err(error), Err(error));
        }
        let folders = self
            .configuration
            .folders()
            .iter()
            .map(|folder| folder.id())
            .collect::<Vec<_>>();
        let peers = self
            .configuration
            .peers()
            .iter()
            .map(|peer| peer.id().clone())
            .collect::<Vec<_>>();
        let (folders, connections) = tokio::join!(
            super::collect_folder_health(&self.client, &folders),
            super::collect_peer_connections(&self.client, &peers),
        );
        if let Err(error) = self.revalidate_roots() {
            self.worker.close_lifeline();
            return (Err(error), Err(error));
        }
        (
            folders.map_err(|_| EngineSessionError::EngineUnavailable),
            connections.map_err(|_| EngineSessionError::EngineUnavailable),
        )
    }

    /// Request stop through the owner lifeline and await bounded reaping.
    pub async fn stop(&mut self) -> Result<StopOutcome, super::EngineSupervisorError> {
        self.worker.stop().await
    }

    /// Close the owner lifeline synchronously before awaiting shutdown or
    /// changing authority. This is idempotent; actual reaping still uses stop.
    pub fn close_lifeline(&mut self) {
        self.worker.close_lifeline();
    }

    fn revalidate_roots(&self) -> Result<(), EngineSessionError> {
        revalidate_capabilities(&self.installation, &self.roots)
    }
}

pub(super) fn validate_initial_scan_health(
    folders: &[uuid::Uuid],
    health: &[super::FolderHealth],
) -> Result<(), EngineSessionError> {
    if folders.is_empty()
        || health.len() != folders.len()
        || health.iter().zip(folders).any(|(observed, expected)| {
            observed.folder != *expected
                || observed.lifecycle == super::FolderLifecycle::Error
                || observed.has_reported_errors()
        })
    {
        return Err(EngineSessionError::EngineUnavailable);
    }
    Ok(())
}

fn revalidate_capabilities(
    installation: &EngineInstallation,
    roots: &[FolderRootLease],
) -> Result<(), EngineSessionError> {
    installation
        .revalidate()
        .map_err(|_| EngineSessionError::InvalidConfiguration)?;
    for root in roots {
        let metadata = std::fs::symlink_metadata(&root.path)
            .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        let held = root
            .file
            .metadata()
            .map_err(|_| EngineSessionError::InvalidConfiguration)?;
        if !metadata.is_dir()
            || (metadata.dev(), metadata.ino()) != root.identity
            || (held.dev(), held.ino()) != root.identity
        {
            return Err(EngineSessionError::InvalidConfiguration);
        }
    }
    Ok(())
}

fn admit_root(
    path: &Path,
    installation: &Path,
    runtime: &Path,
) -> Result<FolderRootLease, EngineSessionError> {
    for private in [installation, runtime] {
        if path.starts_with(private) || private.starts_with(path) {
            return Err(EngineSessionError::InvalidConfiguration);
        }
    }
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| EngineSessionError::InvalidConfiguration)?;
    let file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|_| EngineSessionError::InvalidConfiguration)?;
    Ok(FolderRootLease {
        path: path.to_path_buf(),
        file,
        identity: (metadata.dev(), metadata.ino()),
    })
}
