//! Owner-scoped process supervision for the packaged sync engine.
//!
//! This module deliberately owns one guardian child and one parent lifeline.
//! It never searches for processes, kills a process group, unlinks runtime
//! files, or includes paths, arguments, or operating-system errors in its
//! public diagnostics. The packaged files are expected to be immutable; the
//! path checks are not a promise against an arbitrary same-UID path race.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Seek as _, SeekFrom, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use super::android_saf::{AndroidSafGrantError, AndroidSafGrantRegistry, PasswordObscurer};

pub const MAX_EXECUTABLE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CERTIFICATE_BYTES: u64 = 24 * 1024;
const MAX_KEY_BYTES: u64 = 4 * 1024;
const GUARDIAN_GRACE_MS: &str = "2000";
const STOP_TIMEOUT: Duration = Duration::from_secs(4);

/// Stable errors intentionally omit paths, command lines, secrets, and OS
/// error strings. Callers can attach a local correlation ID at a higher layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineSupervisorError {
    InvalidExecutable,
    ExecutableChanged,
    ExecutableDigestMismatch,
    InvalidRuntimeFile,
    InvalidRuntimeDirectory,
    SpawnFailed,
    ReaperUnavailable,
    WaitFailed,
    OutputTooLarge,
}

/// Controller-owned state kept alive until the exact guardian has been
/// reaped. The value is released by the reaper thread, never by async handle
/// cancellation or worker-handle drop.
pub type WorkerKeepalive = Box<dyn Send + 'static>;

impl fmt::Display for EngineSupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidExecutable => "sync engine executable is invalid",
            Self::ExecutableChanged => "sync engine executable changed",
            Self::ExecutableDigestMismatch => "sync engine executable digest mismatch",
            Self::InvalidRuntimeFile => "sync engine runtime file is invalid",
            Self::InvalidRuntimeDirectory => "sync engine runtime directory is invalid",
            Self::SpawnFailed => "sync engine could not be started",
            Self::ReaperUnavailable => "sync engine reaper is unavailable",
            Self::WaitFailed => "sync engine could not be reaped",
            Self::OutputTooLarge => "sync engine output exceeded its limit",
        })
    }
}

impl std::error::Error for EngineSupervisorError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

struct ExecutableObscurer {
    path: PathBuf,
    identity: FileIdentity,
    expected_sha256: [u8; 32],
}

impl PasswordObscurer for ExecutableObscurer {
    fn obscure(
        &self,
        password: Zeroizing<String>,
    ) -> Result<Zeroizing<String>, AndroidSafGrantError> {
        recheck_path(&self.path, self.identity, self.expected_sha256)
            .map_err(|_| AndroidSafGrantError::Unavailable)?;
        let mut child = Command::new(&self.path)
            .args(["obscure", "-"])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| AndroidSafGrantError::Unavailable)?;
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AndroidSafGrantError::Unavailable);
        };
        stdin
            .write_all(password.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .map_err(|_| AndroidSafGrantError::Unavailable)?;
        drop(stdin);
        let output = child
            .wait_with_output()
            .map_err(|_| AndroidSafGrantError::Unavailable)?;
        if !output.status.success() || output.stdout.len() > 4097 {
            return Err(AndroidSafGrantError::Unavailable);
        }
        let value =
            String::from_utf8(output.stdout).map_err(|_| AndroidSafGrantError::Unavailable)?;
        let value = value.strip_suffix('\n').unwrap_or(&value);
        if value.is_empty()
            || value.len() > 4096
            || value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
        {
            return Err(AndroidSafGrantError::Unavailable);
        }
        Ok(Zeroizing::new(value.to_owned()))
    }
}

/// A manifest-pinned executable opened through an `O_NOFOLLOW` descriptor.
/// The descriptor is retained for the lifetime of this value so the initial
/// digest is of the object that was actually opened, not a later path lookup.
pub struct VerifiedEngineExecutable {
    path: PathBuf,
    file: File,
    identity: FileIdentity,
    expected_sha256: [u8; 32],
    android_saf_grants: AndroidSafGrantRegistry,
}

impl fmt::Debug for VerifiedEngineExecutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VerifiedEngineExecutable([PRIVATE])")
    }
}

impl VerifiedEngineExecutable {
    /// Verify an absolute executable's trusted owner, safe mode and pinned
    /// manifest digest. The caller must retain this value until launch.
    pub fn open(
        path: impl Into<PathBuf>,
        expected_sha256: [u8; 32],
    ) -> Result<Self, EngineSupervisorError> {
        let path = path.into();
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(EngineSupervisorError::InvalidExecutable);
        }

        let file = open_nofollow(&path).map_err(|_| EngineSupervisorError::InvalidExecutable)?;
        let metadata = file
            .metadata()
            .map_err(|_| EngineSupervisorError::InvalidExecutable)?;
        validate_executable_metadata(&metadata)?;
        let identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        let mut file = file;
        let digest = digest_file(&mut file, MAX_EXECUTABLE_BYTES)
            .map_err(|_| EngineSupervisorError::InvalidExecutable)?;
        let after_hash = file
            .metadata()
            .map_err(|_| EngineSupervisorError::InvalidExecutable)?;
        validate_executable_metadata(&after_hash)?;
        if (FileIdentity {
            device: after_hash.dev(),
            inode: after_hash.ino(),
        }) != identity
        {
            return Err(EngineSupervisorError::ExecutableChanged);
        }
        if digest != expected_sha256 {
            return Err(EngineSupervisorError::ExecutableDigestMismatch);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| EngineSupervisorError::InvalidExecutable)?;
        let obscurer = Arc::new(ExecutableObscurer {
            path: path.clone(),
            identity,
            expected_sha256,
        });
        Ok(Self {
            path,
            file,
            identity,
            expected_sha256,
            android_saf_grants: AndroidSafGrantRegistry::with_obscurer(obscurer),
        })
    }

    /// Parse the lowercase or uppercase hexadecimal form used by manifests.
    pub fn parse_sha256_hex(value: &str) -> Result<[u8; 32], EngineSupervisorError> {
        if value.len() != 64 {
            return Err(EngineSupervisorError::ExecutableDigestMismatch);
        }
        let mut digest = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high =
                hex_nibble(pair[0]).ok_or(EngineSupervisorError::ExecutableDigestMismatch)?;
            let low = hex_nibble(pair[1]).ok_or(EngineSupervisorError::ExecutableDigestMismatch)?;
            digest[index] = (high << 4) | low;
        }
        Ok(digest)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runtime-only SAF grants associated with this verified worker handle.
    #[must_use]
    pub fn android_saf_grants(&self) -> AndroidSafGrantRegistry {
        self.android_saf_grants.clone()
    }

    /// Recheck the pinned object immediately before constructing the child.
    /// This catches ordinary replacement of a packaged file. It does not
    /// claim to close every same-UID path race between this check and exec.
    fn recheck_at_spawn(&self) -> Result<(), EngineSupervisorError> {
        let held = self
            .file
            .metadata()
            .map_err(|_| EngineSupervisorError::ExecutableChanged)?;
        if (FileIdentity {
            device: held.dev(),
            inode: held.ino(),
        }) != self.identity
        {
            return Err(EngineSupervisorError::ExecutableChanged);
        }
        let mut current =
            open_nofollow(&self.path).map_err(|_| EngineSupervisorError::ExecutableChanged)?;
        let metadata = current
            .metadata()
            .map_err(|_| EngineSupervisorError::ExecutableChanged)?;
        validate_executable_metadata(&metadata)?;
        let identity = FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if identity != self.identity {
            return Err(EngineSupervisorError::ExecutableChanged);
        }
        let digest = digest_file(&mut current, MAX_EXECUTABLE_BYTES)
            .map_err(|_| EngineSupervisorError::ExecutableChanged)?;
        if digest != self.expected_sha256 {
            return Err(EngineSupervisorError::ExecutableChanged);
        }
        let after_hash = current
            .metadata()
            .map_err(|_| EngineSupervisorError::ExecutableChanged)?;
        validate_executable_metadata(&after_hash)
            .map_err(|_| EngineSupervisorError::ExecutableChanged)?;
        if (FileIdentity {
            device: after_hash.dev(),
            inode: after_hash.ino(),
        }) != identity
        {
            return Err(EngineSupervisorError::ExecutableChanged);
        }
        Ok(())
    }

    pub(super) fn recheck(&self) -> Result<(), EngineSupervisorError> {
        self.recheck_at_spawn()
    }

    pub(super) fn try_clone(&self) -> Result<Self, EngineSupervisorError> {
        Ok(Self {
            path: self.path.clone(),
            file: self
                .file
                .try_clone()
                .map_err(|_| EngineSupervisorError::InvalidExecutable)?,
            identity: self.identity,
            expected_sha256: self.expected_sha256,
            android_saf_grants: self.android_saf_grants.clone(),
        })
    }
}

fn recheck_path(
    path: &Path,
    expected_identity: FileIdentity,
    expected_sha256: [u8; 32],
) -> Result<(), EngineSupervisorError> {
    let mut current = open_nofollow(path).map_err(|_| EngineSupervisorError::ExecutableChanged)?;
    let metadata = current
        .metadata()
        .map_err(|_| EngineSupervisorError::ExecutableChanged)?;
    validate_executable_metadata(&metadata)?;
    if (FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }) != expected_identity
        || digest_file(&mut current, MAX_EXECUTABLE_BYTES)
            .map_err(|_| EngineSupervisorError::ExecutableChanged)?
            != expected_sha256
    {
        return Err(EngineSupervisorError::ExecutableChanged);
    }
    Ok(())
}

/// Result of asking the guardian to stop. `StillStopping` is intentionally not
/// terminal: the same receiver remains retained and `stop` may be retried.
#[derive(Debug)]
pub enum StopOutcome {
    Exited(ExitStatus),
    StillStopping,
}

/// One exact guardian child with a dedicated OS reaper thread. Dropping this
/// value closes its stdin lifeline and leaves the guardian to perform its
/// bounded TERM/KILL cleanup; it never sends a numeric-PID or group kill.
pub struct OwnedEngineWorker {
    lifeline: Option<ChildStdin>,
    reaped: Option<oneshot::Receiver<Result<ExitStatus, EngineSupervisorError>>>,
    stop_started: bool,
    terminal_status: Option<ExitStatus>,
}

/// A guarded one-shot rclone command. Dropping this value, including through
/// async cancellation, closes the guardian lifeline. Capture threads continue
/// draining bounded output until the exact guardian child has been reaped.
pub(super) struct OwnedRcloneCommand {
    worker: OwnedEngineWorker,
    stdout: oneshot::Receiver<Result<BoundedOutput, EngineSupervisorError>>,
    stderr: oneshot::Receiver<Result<BoundedOutput, EngineSupervisorError>>,
}

pub(super) struct RcloneCommandOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

struct SpawnedRclone {
    worker: OwnedEngineWorker,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

struct BoundedOutput {
    bytes: Vec<u8>,
    exceeded_limit: bool,
}

impl fmt::Debug for OwnedEngineWorker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedEngineWorker([PRIVATE])")
    }
}

impl OwnedEngineWorker {
    /// Launch a verified rclone receiver through the same exact guardian and
    /// reaper contract. Arguments and environment come only from controller
    /// validated values.
    pub(super) fn launch_rclone(
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        args: &[OsString],
        environment: &[(OsString, OsString)],
        runtime_dir: &Path,
        keepalive: WorkerKeepalive,
    ) -> Result<Self, EngineSupervisorError> {
        Ok(Self::launch_rclone_common(
            guardian,
            engine,
            args,
            environment,
            runtime_dir,
            false,
            keepalive,
        )?
        .worker)
    }

    fn launch_rclone_common(
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        args: &[OsString],
        environment: &[(OsString, OsString)],
        runtime_dir: &Path,
        capture_output: bool,
        keepalive: WorkerKeepalive,
    ) -> Result<SpawnedRclone, EngineSupervisorError> {
        guardian.recheck_at_spawn()?;
        engine.recheck_at_spawn()?;
        let runtime_dir = fs::canonicalize(runtime_dir)
            .map_err(|_| EngineSupervisorError::InvalidRuntimeDirectory)?;
        canonical_private_directory(&runtime_dir)?;
        let (child_tx, child_rx) = mpsc::sync_channel::<(Child, WorkerKeepalive)>(1);
        let (result_tx, result_rx) = oneshot::channel();
        spawn_reaper(child_rx, result_tx)?;
        let mut command = Command::new(guardian.path());
        command
            .arg("--grace-ms")
            .arg(GUARDIAN_GRACE_MS)
            .arg("--")
            .arg(engine.path())
            .args(args)
            .env_clear()
            .envs(environment.iter().cloned())
            .env("TMPDIR", &runtime_dir)
            .env("GOMAXPROCS", "2")
            .current_dir(&runtime_dir)
            .stdin(Stdio::piped());
        if capture_output {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        } else {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        #[cfg(not(target_os = "android"))]
        if let Some(home) = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .filter(|home| home.is_absolute() && home.is_dir())
        {
            command.env("HOME", home);
        }
        let mut child = command
            .spawn()
            .map_err(|_| EngineSupervisorError::SpawnFailed)?;
        let lifeline = child
            .stdin
            .take()
            .ok_or(EngineSupervisorError::SpawnFailed)?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        if capture_output && (stdout.is_none() || stderr.is_none()) {
            reap_after_handoff_failure(&mut child, Some(lifeline), keepalive);
            return Err(EngineSupervisorError::SpawnFailed);
        }
        if let Err(error) = child_tx.send((child, keepalive)) {
            let (mut child, keepalive) = error.0;
            reap_after_handoff_failure(&mut child, Some(lifeline), keepalive);
            return Err(EngineSupervisorError::ReaperUnavailable);
        }
        Ok(SpawnedRclone {
            worker: Self {
                lifeline: Some(lifeline),
                reaped: Some(result_rx),
                stop_started: false,
                terminal_status: None,
            },
            stdout,
            stderr,
        })
    }

    /// Launch the exact verified guardian and engine with the fixed worker
    /// contract. Runtime files are existing controller-owned inputs; this
    /// primitive never creates defaults or removes them.
    pub fn launch(
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        config_dir: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        keepalive: WorkerKeepalive,
    ) -> Result<Self, EngineSupervisorError> {
        let config_dir = config_dir.into();
        let data_dir = data_dir.into();
        let (config_dir, data_dir) = validate_runtime_inputs(&config_dir, &data_dir)?;
        guardian.recheck_at_spawn()?;
        engine.recheck_at_spawn()?;

        // Start the reaper and establish its handoff channel before spawning.
        // If the child handoff fails, the sending side retains the exact Child
        // and synchronously closes/reaps it before returning an error.
        let (child_tx, child_rx) = mpsc::sync_channel::<(Child, WorkerKeepalive)>(1);
        let (result_tx, result_rx) = oneshot::channel();
        spawn_reaper(child_rx, result_tx)?;

        // The pinned upstream locations module resolves the real OS home at
        // process initialization, even with explicit config/data flags. Keep
        // this one non-secret OS value on desktop/server hosts. Android app
        // processes omit HOME; Go uses its Android default there. Explicit
        // config/data paths below still keep all worker state app-private.
        #[cfg(not(target_os = "android"))]
        let os_home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .filter(|home| home.is_absolute() && home.is_dir())
            .ok_or(EngineSupervisorError::InvalidRuntimeDirectory)?;
        let mut command = Command::new(guardian.path());
        command
            .arg("--grace-ms")
            .arg(GUARDIAN_GRACE_MS)
            .arg("--")
            .arg(engine.path())
            .arg("--config")
            .arg(&config_dir)
            .arg("--data")
            .arg(&data_dir)
            .arg("serve")
            .arg("--no-browser")
            .arg("--no-restart")
            .arg("--no-upgrade")
            .arg("--no-port-probing")
            .env_clear()
            .env("STMONITORED", "1")
            .env("STNOUPGRADE", "1")
            .env("TMPDIR", &config_dir)
            .env("GOMAXPROCS", "2")
            .current_dir(&data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(not(target_os = "android"))]
        command.env("HOME", os_home);

        let mut child = command
            .spawn()
            .map_err(|_| EngineSupervisorError::SpawnFailed)?;
        let lifeline = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                reap_after_handoff_failure(&mut child, None, keepalive);
                return Err(EngineSupervisorError::SpawnFailed);
            }
        };
        if let Err(error) = child_tx.send((child, keepalive)) {
            let (mut child, keepalive) = error.0;
            // Dropping stdin closes the guardian's lifeline before reaping the
            // exact owned child. No process lookup or group kill is involved.
            reap_after_handoff_failure(&mut child, Some(lifeline), keepalive);
            return Err(EngineSupervisorError::ReaperUnavailable);
        }
        Ok(Self {
            lifeline: Some(lifeline),
            reaped: Some(result_rx),
            stop_started: false,
            terminal_status: None,
        })
    }

    /// Close the guardian lifeline. Repeated calls are harmless.
    pub fn close_lifeline(&mut self) {
        self.lifeline.take();
        self.stop_started = true;
    }

    /// Close the lifeline once and wait for the dedicated reaper for a bounded
    /// period. A timeout retains the receiver and returns `StillStopping`.
    pub async fn stop(&mut self) -> Result<StopOutcome, EngineSupervisorError> {
        self.close_lifeline();
        if let Some(status) = self.try_status()? {
            return Ok(StopOutcome::Exited(status));
        }
        let receiver = self
            .reaped
            .as_mut()
            .ok_or(EngineSupervisorError::ReaperUnavailable)?;
        match tokio::time::timeout(STOP_TIMEOUT, receiver).await {
            Ok(Ok(Ok(status))) => {
                self.terminal_status = Some(status);
                let status = *self
                    .terminal_status
                    .as_ref()
                    .expect("terminal status stored");
                self.reaped.take();
                Ok(StopOutcome::Exited(status))
            }
            Ok(Ok(Err(error))) => {
                self.reaped.take();
                Err(error)
            }
            Ok(Err(_)) => {
                self.reaped.take();
                Err(EngineSupervisorError::ReaperUnavailable)
            }
            Err(_) => Ok(StopOutcome::StillStopping),
        }
    }

    /// Poll the reaper without closing the lifeline. A returned status is
    /// retained, so subsequent polls and stops remain terminal and repeatable.
    pub fn try_status(&mut self) -> Result<Option<ExitStatus>, EngineSupervisorError> {
        if let Some(status) = &self.terminal_status {
            return Ok(Some(*status));
        }
        let receiver = self
            .reaped
            .as_mut()
            .ok_or(EngineSupervisorError::ReaperUnavailable)?;
        match receiver.try_recv() {
            Ok(Ok(status)) => {
                self.terminal_status = Some(status);
                let status = *self
                    .terminal_status
                    .as_ref()
                    .expect("terminal status stored");
                self.reaped.take();
                Ok(Some(status))
            }
            Ok(Err(error)) => {
                self.reaped.take();
                Err(error)
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => Ok(None),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.reaped.take();
                Err(EngineSupervisorError::ReaperUnavailable)
            }
        }
    }

    #[must_use]
    pub fn stop_started(&self) -> bool {
        self.stop_started
    }

    async fn wait_for_exit(mut self) -> Result<ExitStatus, EngineSupervisorError> {
        let receiver = self
            .reaped
            .as_mut()
            .ok_or(EngineSupervisorError::ReaperUnavailable)?;
        let status = receiver
            .await
            .map_err(|_| EngineSupervisorError::ReaperUnavailable)??;
        self.terminal_status = Some(status);
        self.reaped.take();
        Ok(status)
    }
}

impl OwnedRcloneCommand {
    pub(super) fn launch(
        guardian: &VerifiedEngineExecutable,
        engine: &VerifiedEngineExecutable,
        args: &[OsString],
        environment: &[(OsString, OsString)],
        runtime_dir: &Path,
        max_output_bytes: u64,
        keepalive: WorkerKeepalive,
    ) -> Result<Self, EngineSupervisorError> {
        let spawned = OwnedEngineWorker::launch_rclone_common(
            guardian,
            engine,
            args,
            environment,
            runtime_dir,
            true,
            keepalive,
        )?;
        let stdout = spawn_bounded_output_reader(
            "covalent-rclone-stdout",
            spawned.stdout.ok_or(EngineSupervisorError::SpawnFailed)?,
            max_output_bytes,
        )?;
        let stderr = spawn_bounded_output_reader(
            "covalent-rclone-stderr",
            spawned.stderr.ok_or(EngineSupervisorError::SpawnFailed)?,
            max_output_bytes,
        )?;
        Ok(Self {
            worker: spawned.worker,
            stdout,
            stderr,
        })
    }

    /// Wait for natural command completion. This keeps the lifeline open while
    /// waiting; cancellation drops `self` and closes it through the worker.
    pub(super) async fn wait(self) -> Result<RcloneCommandOutput, EngineSupervisorError> {
        let Self {
            worker,
            stdout,
            stderr,
        } = self;
        let status = worker.wait_for_exit().await?;
        let (stdout, stderr) = tokio::join!(stdout, stderr);
        let stdout = stdout.map_err(|_| EngineSupervisorError::WaitFailed)??;
        let stderr = stderr.map_err(|_| EngineSupervisorError::WaitFailed)??;
        if stdout.exceeded_limit || stderr.exceeded_limit {
            return Err(EngineSupervisorError::OutputTooLarge);
        }
        Ok(RcloneCommandOutput {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        })
    }
}

impl Drop for OwnedEngineWorker {
    fn drop(&mut self) {
        // The reaper owns the Child. Closing this pipe is the only action Drop
        // takes; it does not kill the guardian and does not touch runtime files.
        self.lifeline.take();
    }
}

fn spawn_reaper(
    child_rx: Receiver<(Child, WorkerKeepalive)>,
    result_tx: oneshot::Sender<Result<ExitStatus, EngineSupervisorError>>,
) -> Result<(), EngineSupervisorError> {
    thread::Builder::new()
        .name("covalent-engine-reaper".to_owned())
        .spawn(move || {
            let Ok((mut child, keepalive)) = child_rx.recv() else {
                let _ = result_tx.send(Err(EngineSupervisorError::ReaperUnavailable));
                return;
            };
            let status = wait_until_reaped(&mut child);
            drop(keepalive);
            let _ = result_tx.send(Ok(status));
        })
        .map(|_| ())
        .map_err(|_| EngineSupervisorError::ReaperUnavailable)
}

fn reap_after_handoff_failure(
    child: &mut Child,
    lifeline: Option<ChildStdin>,
    keepalive: WorkerKeepalive,
) {
    drop(lifeline);
    let _ = wait_until_reaped(child);
    drop(keepalive);
}

fn wait_until_reaped(child: &mut Child) -> ExitStatus {
    loop {
        match child.wait() {
            Ok(status) => return status,
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn spawn_bounded_output_reader<R: Read + Send + 'static>(
    name: &str,
    mut reader: R,
    max_output_bytes: u64,
) -> Result<oneshot::Receiver<Result<BoundedOutput, EngineSupervisorError>>, EngineSupervisorError>
{
    let (result_tx, result_rx) = oneshot::channel();
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let mut bytes = Vec::with_capacity(max_output_bytes.min(8192) as usize);
            let mut buffer = [0_u8; 8192];
            let mut exceeded_limit = false;
            loop {
                let count = match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => count,
                    Err(_) => {
                        let _ = result_tx.send(Err(EngineSupervisorError::WaitFailed));
                        return;
                    }
                };
                let remaining = max_output_bytes.saturating_sub(bytes.len() as u64);
                let retained = (count as u64).min(remaining) as usize;
                bytes.extend_from_slice(&buffer[..retained]);
                exceeded_limit |= count > retained;
            }
            let _ = result_tx.send(Ok(BoundedOutput {
                bytes,
                exceeded_limit,
            }));
        })
        .map_err(|_| EngineSupervisorError::ReaperUnavailable)?;
    Ok(result_rx)
}

fn validate_runtime_inputs(
    config_dir: &Path,
    data_dir: &Path,
) -> Result<(PathBuf, PathBuf), EngineSupervisorError> {
    let config_dir = canonical_private_directory(config_dir)?;
    let data_dir = canonical_private_directory(data_dir)?;
    // The config directory must contain the exact engine configuration and
    // identity material. Do not generate or repair any of these files here.
    for (name, max_size) in [
        ("config.xml", MAX_CONFIG_BYTES),
        ("cert.pem", MAX_CERTIFICATE_BYTES),
        ("key.pem", MAX_KEY_BYTES),
    ] {
        let path = config_dir.join(name);
        let _ = canonical_private_file(&path, 0o600, max_size)?;
    }
    Ok((config_dir, data_dir))
}

fn canonical_private_file(
    path: &Path,
    mode: u32,
    max_size: u64,
) -> Result<PathBuf, EngineSupervisorError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(EngineSupervisorError::InvalidRuntimeFile);
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|_| EngineSupervisorError::InvalidRuntimeFile)?;
    validate_private_file_metadata(&metadata, mode, max_size)?;
    let canonical =
        fs::canonicalize(path).map_err(|_| EngineSupervisorError::InvalidRuntimeFile)?;
    Ok(canonical)
}

fn canonical_private_directory(path: &Path) -> Result<PathBuf, EngineSupervisorError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(EngineSupervisorError::InvalidRuntimeDirectory);
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|_| EngineSupervisorError::InvalidRuntimeDirectory)?;
    let uid = rustix::process::geteuid().as_raw();
    if !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || metadata.mode() & 0o6000 != 0
    {
        return Err(EngineSupervisorError::InvalidRuntimeDirectory);
    }
    let canonical =
        fs::canonicalize(path).map_err(|_| EngineSupervisorError::InvalidRuntimeDirectory)?;
    Ok(canonical)
}

fn validate_private_file_metadata(
    metadata: &fs::Metadata,
    required_mode: u32,
    max_size: u64,
) -> Result<(), EngineSupervisorError> {
    let uid = rustix::process::geteuid().as_raw();
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o7777 != required_mode
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > max_size
    {
        return Err(EngineSupervisorError::InvalidRuntimeFile);
    }
    Ok(())
}

fn validate_executable_metadata(metadata: &fs::Metadata) -> Result<(), EngineSupervisorError> {
    let uid = rustix::process::geteuid().as_raw();
    let mode = metadata.mode();
    if !metadata.is_file()
        || !is_trusted_executable_owner(metadata.uid(), uid)
        || mode & 0o022 != 0
        || mode & 0o6000 != 0
        || mode & 0o111 == 0
        || metadata.len() > MAX_EXECUTABLE_BYTES
    {
        return Err(EngineSupervisorError::InvalidExecutable);
    }
    Ok(())
}

fn is_trusted_executable_owner(owner: u32, current: u32) -> bool {
    // Android's package installer owns extracted native libraries as AID_SYSTEM.
    // This allowance applies only to executable ownership; private state still
    // requires the app UID, and every executable keeps the hash and mode checks.
    owner == 0 || owner == current || (cfg!(target_os = "android") && owner == 1000)
}

fn open_nofollow(path: &Path) -> std::io::Result<File> {
    // rustix exposes O_NOFOLLOW on Darwin and Linux, avoiding a final
    // symlink during the descriptor acquisition used for digest verification.
    let fd: OwnedFd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )?;
    Ok(File::from(fd))
}

fn digest_file(file: &mut File, max_size: u64) -> std::io::Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let remaining = max_size.saturating_add(1).saturating_sub(total);
        if remaining == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "digest input exceeds bound",
            ));
        }
        let read_limit = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = file.read(&mut buffer[..read_limit])?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > max_size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "digest input exceeds bound",
            ));
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
