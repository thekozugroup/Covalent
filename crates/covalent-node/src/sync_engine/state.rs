//! Atomic local sharing snapshots, authenticated to one engine installation.
//!
//! Snapshot bytes contain private local configuration, never engine secrets.
//! They are visible only inside owner-only state. A wrapped digest authenticates
//! the complete payload and revision; it does not encrypt the payload. The
//! sharing coordinator owns schema/authority validation before any worker starts.

use std::fmt;
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::sync::Arc;

use covalent_core::sync::state_dir::{PrivateStateFile, StateKey};
use covalent_core::{KeyProtector, WrappedSecret, state_secret_context};
use rustix::fs::{AtFlags, Mode, OFlags, fchmod, fsync, open, openat, renameat, statat, unlinkat};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use super::EngineInstallation;

const RECORD_NAME: &str = "folder-sharing.v1";
const STAGED_NAME: &str = "folder-sharing.staged.v1";
const PURPOSE: &str = "folder-sharing-state";
const MAX_PAYLOAD: usize = 4 * 1024 * 1024;
const MAX_RECORD: u64 = 8 * 1024 * 1024;

/// Fixed state errors never disclose paths, configuration, or key material.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineStateError {
    /// The private installation or expected current snapshot is unavailable.
    Unavailable,
    /// The snapshot is missing, malformed, copied, or cannot be authenticated.
    InvalidState,
    /// The proposed payload is not bounded, valid JSON.
    InvalidPayload,
    /// Another state handle committed a revision; reopen before retrying.
    StaleRevision,
    /// A write may have reached disk; reopen and reconcile before any launch.
    PersistenceUncertain,
}

impl fmt::Display for EngineStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "folder sharing state is unavailable",
            Self::InvalidState => "folder sharing state cannot be verified",
            Self::InvalidPayload => "folder sharing configuration is invalid",
            Self::StaleRevision => "folder sharing state changed; reopen it",
            Self::PersistenceUncertain => "folder sharing state needs recovery",
        })
    }
}

impl std::error::Error for EngineStateError {}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    schema_version: u16,
    engine_device_id: String,
    revision: u64,
    payload: String,
    authentication: WrappedSecret,
}

/// An authenticated revision of local desired sharing state. This grants no
/// remote authority and never launches a worker. Complete rollback of an older
/// valid installation snapshot is outside the platform key protector's scope.
/// The coordinator must recheck current Covalent revocations at every startup.
pub struct EngineStateStore {
    installation: Arc<EngineInstallation>,
    protector: Arc<dyn KeyProtector>,
    current: PrivateStateFile,
    digest: [u8; 32],
    revision: u64,
    payload: Zeroizing<Vec<u8>>,
    uncertain: bool,
}

impl fmt::Debug for EngineStateStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EngineStateStore([PRIVATE])")
    }
}

impl EngineStateStore {
    /// Create the first snapshot without replacing any incumbent. Called only
    /// during explicit new-installation initialization, before any worker.
    pub fn create(
        installation: Arc<EngineInstallation>,
        protector: Arc<dyn KeyProtector>,
        payload: &[u8],
    ) -> Result<Self, EngineStateError> {
        let guard = installation
            .state_writer()
            .lock()
            .map_err(|_| EngineStateError::Unavailable)?;
        installation
            .revalidate()
            .map_err(|_| EngineStateError::Unavailable)?;
        let bytes = encode(&installation, protector.as_ref(), 1, payload)?;
        installation
            .state_directory()
            .create_new_file(installation.state_lock(), &key()?, &bytes, MAX_RECORD)
            .map_err(|_| EngineStateError::PersistenceUncertain)?;
        drop(guard);
        Self::open(installation, protector)
    }

    /// Open only existing authenticated state. Missing or damaged state is
    /// never replaced by an empty configuration or an automatic migration.
    pub fn open(
        installation: Arc<EngineInstallation>,
        protector: Arc<dyn KeyProtector>,
    ) -> Result<Self, EngineStateError> {
        let guard = installation
            .state_writer()
            .lock()
            .map_err(|_| EngineStateError::Unavailable)?;
        installation
            .revalidate()
            .map_err(|_| EngineStateError::Unavailable)?;
        let current = installation
            .state_directory()
            .open_file(&key()?, MAX_RECORD)
            .map_err(|_| EngineStateError::InvalidState)?;
        let bytes = Zeroizing::new(
            current
                .read_all()
                .map_err(|_| EngineStateError::InvalidState)?,
        );
        let (revision, payload) = decode(&installation, protector.as_ref(), &bytes)?;
        let digest = Sha256::digest(&bytes).into();
        reconcile_staged_record(&installation)?;
        // Reopen is also the recovery boundary for a rename whose parent sync
        // was interrupted. Authenticate first, then make this exact survivor
        // durable before it may be used to authorize a new worker.
        current
            .sync_all(installation.state_lock())
            .and_then(|()| {
                installation
                    .state_directory()
                    .sync(installation.state_lock())
            })
            .map_err(|_| EngineStateError::PersistenceUncertain)?;
        if current
            .read_all()
            .map_err(|_| EngineStateError::PersistenceUncertain)?
            != *bytes
        {
            return Err(EngineStateError::PersistenceUncertain);
        }
        installation
            .revalidate()
            .map_err(|_| EngineStateError::Unavailable)?;
        drop(guard);
        Ok(Self {
            installation,
            protector,
            current,
            digest,
            revision,
            payload,
            uncertain: false,
        })
    }

    /// Current durable revision; used for local optimistic request sequencing.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Borrow the authenticated payload after checking that its directory entry
    /// and complete bytes remain current. The caller must validate its schema.
    /// The lifecycle coordinator must serialize this observation through worker
    /// launch against all mutations; this borrowed snapshot is no launch lease.
    pub fn payload(&self) -> Result<&[u8], EngineStateError> {
        self.revalidate()?;
        Ok(&self.payload)
    }

    /// Persist the complete next desired configuration. Success requires file
    /// and directory fsync and exact readback; memory advances only afterward.
    /// A reported uncertain write permanently stops this handle from reuse.
    pub fn replace(&mut self, payload: &[u8]) -> Result<(), EngineStateError> {
        self.replace_at_boundaries(payload, |_| Ok(()))
    }

    fn replace_at_boundaries(
        &mut self,
        payload: &[u8],
        mut boundary: impl FnMut(WriteBoundary) -> Result<(), EngineStateError>,
    ) -> Result<(), EngineStateError> {
        let installation = Arc::clone(&self.installation);
        let _guard = installation
            .state_writer()
            .lock()
            .map_err(|_| EngineStateError::Unavailable)?;
        self.revalidate()?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(EngineStateError::InvalidState)?;
        let bytes = encode(&installation, self.protector.as_ref(), revision, payload)?;
        let directory = File::from(
            open(
                installation.root(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| EngineStateError::Unavailable)?,
        );
        installation
            .revalidate()
            .map_err(|_| EngineStateError::Unavailable)?;
        // From here a failure is conservatively uncertain, including a partial
        // staging write. A worker must remain stopped until explicit reopen.
        self.uncertain = true;
        let mut staged = StagedRecord::create(directory)?;
        staged
            .file
            .write_all(&bytes)
            .and_then(|()| staged.file.sync_all())
            .map_err(|_| EngineStateError::PersistenceUncertain)?;
        boundary(WriteBoundary::StagedDurable)?;
        // Observe both the anchored incumbent and the installation again just
        // before rename. Cooperating handles are serialized by state_writer.
        let current = self
            .current
            .read_all()
            .map_err(|_| EngineStateError::PersistenceUncertain)?;
        if <[u8; 32]>::from(Sha256::digest(&current)) != self.digest {
            return Err(EngineStateError::PersistenceUncertain);
        }
        installation
            .revalidate()
            .map_err(|_| EngineStateError::PersistenceUncertain)?;
        staged.promote(&mut boundary)?;
        let next = installation
            .state_directory()
            .open_file(&key()?, MAX_RECORD)
            .map_err(|_| EngineStateError::PersistenceUncertain)?;
        let readback = Zeroizing::new(
            next.read_all()
                .map_err(|_| EngineStateError::PersistenceUncertain)?,
        );
        if readback.as_slice() != bytes.as_slice() {
            return Err(EngineStateError::PersistenceUncertain);
        }
        installation
            .revalidate()
            .map_err(|_| EngineStateError::PersistenceUncertain)?;
        self.current = next;
        self.digest = Sha256::digest(&bytes).into();
        self.revision = revision;
        self.payload = Zeroizing::new(payload.to_vec());
        self.uncertain = false;
        Ok(())
    }

    fn revalidate(&self) -> Result<(), EngineStateError> {
        if self.uncertain {
            return Err(EngineStateError::PersistenceUncertain);
        }
        self.installation
            .revalidate()
            .map_err(|_| EngineStateError::Unavailable)?;
        let bytes = Zeroizing::new(
            self.current
                .read_all()
                .map_err(|_| EngineStateError::StaleRevision)?,
        );
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != self.digest {
            return Err(EngineStateError::StaleRevision);
        }
        Ok(())
    }
}

fn key() -> Result<StateKey, EngineStateError> {
    StateKey::new(RECORD_NAME).map_err(|_| EngineStateError::InvalidState)
}

fn authentication_bytes(revision: u64, payload: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(40));
    bytes.extend_from_slice(&revision.to_be_bytes());
    bytes.extend_from_slice(&Sha256::digest(payload));
    bytes
}

fn validate_payload(payload: &[u8]) -> Result<&str, EngineStateError> {
    if payload.is_empty() || payload.len() > MAX_PAYLOAD {
        return Err(EngineStateError::InvalidPayload);
    }
    let mut decoder = serde_json::Deserializer::from_slice(payload);
    serde::de::IgnoredAny::deserialize(&mut decoder)
        .map_err(|_| EngineStateError::InvalidPayload)?;
    decoder
        .end()
        .map_err(|_| EngineStateError::InvalidPayload)?;
    std::str::from_utf8(payload).map_err(|_| EngineStateError::InvalidPayload)
}

fn encode(
    installation: &EngineInstallation,
    protector: &dyn KeyProtector,
    revision: u64,
    payload: &[u8],
) -> Result<Zeroizing<Vec<u8>>, EngineStateError> {
    let payload_text = validate_payload(payload)?;
    let context = state_secret_context(installation.root(), RECORD_NAME);
    let authentication = WrappedSecret::protect(
        protector,
        PURPOSE,
        &context,
        authentication_bytes(revision, payload),
    )
    .map_err(|_| EngineStateError::InvalidState)?;
    let record = Record {
        schema_version: 1,
        engine_device_id: installation.device_id().as_str().to_owned(),
        revision,
        payload: payload_text.to_owned(),
        authentication,
    };
    let bytes =
        Zeroizing::new(serde_json::to_vec(&record).map_err(|_| EngineStateError::InvalidPayload)?);
    if bytes.len() as u64 > MAX_RECORD {
        return Err(EngineStateError::InvalidPayload);
    }
    Ok(bytes)
}

fn decode(
    installation: &EngineInstallation,
    protector: &dyn KeyProtector,
    bytes: &[u8],
) -> Result<(u64, Zeroizing<Vec<u8>>), EngineStateError> {
    let record: Record =
        serde_json::from_slice(bytes).map_err(|_| EngineStateError::InvalidState)?;
    if record.schema_version != 1
        || record.revision == 0
        || record.engine_device_id != installation.device_id().as_str()
    {
        return Err(EngineStateError::InvalidState);
    }
    let payload = Zeroizing::new(record.payload.into_bytes());
    validate_payload(&payload).map_err(|_| EngineStateError::InvalidState)?;
    let context = state_secret_context(installation.root(), RECORD_NAME);
    let opened = record
        .authentication
        .open(protector, PURPOSE, &context)
        .map_err(|_| EngineStateError::InvalidState)?;
    if *opened != *authentication_bytes(record.revision, &payload) {
        return Err(EngineStateError::InvalidState);
    }
    Ok((record.revision, payload))
}

struct StagedRecord {
    directory: File,
    file: File,
    name: String,
    identity: (u64, u64),
    promoted: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WriteBoundary {
    StagedDurable,
    Renamed,
}

impl StagedRecord {
    fn create(directory: File) -> Result<Self, EngineStateError> {
        let name = STAGED_NAME.to_owned();
        let fd = openat(
            &directory,
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| EngineStateError::PersistenceUncertain)?;
        let file = File::from(fd);
        let metadata = file
            .metadata()
            .map_err(|_| EngineStateError::PersistenceUncertain)?;
        let staged = Self {
            directory,
            file,
            name,
            identity: (metadata.dev(), metadata.ino()),
            promoted: false,
        };
        fchmod(&staged.file, Mode::from_raw_mode(0o600))
            .map_err(|_| EngineStateError::PersistenceUncertain)?;
        Ok(staged)
    }

    fn promote(
        &mut self,
        boundary: &mut impl FnMut(WriteBoundary) -> Result<(), EngineStateError>,
    ) -> Result<(), EngineStateError> {
        if !self.is_current() {
            return Err(EngineStateError::PersistenceUncertain);
        }
        renameat(
            &self.directory,
            self.name.as_str(),
            &self.directory,
            RECORD_NAME,
        )
        .map_err(|_| EngineStateError::PersistenceUncertain)?;
        self.promoted = true;
        boundary(WriteBoundary::Renamed)?;
        fsync(&self.directory).map_err(|_| EngineStateError::PersistenceUncertain)
    }

    fn is_current(&self) -> bool {
        statat(
            &self.directory,
            self.name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .is_ok_and(|s| {
            (s.st_dev as u64, s.st_ino) == self.identity
                && s.st_nlink == 1
                && rustix::fs::FileType::from_raw_mode(s.st_mode)
                    == rustix::fs::FileType::RegularFile
        })
    }
}

impl Drop for StagedRecord {
    fn drop(&mut self) {
        if !self.promoted && self.is_current() {
            let _ = unlinkat(&self.directory, self.name.as_str(), AtFlags::empty());
            let _ = fsync(&self.directory);
        }
    }
}

// One fixed staging slot bounds crash remnants independently of process Drop.
// It is never promoted during recovery: the authenticated current record is
// authoritative. Refuse unsafe/remapped remnants without removing them.
fn reconcile_staged_record(installation: &EngineInstallation) -> Result<(), EngineStateError> {
    let directory = File::from(
        open(
            installation.root(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| EngineStateError::Unavailable)?,
    );
    installation
        .revalidate()
        .map_err(|_| EngineStateError::Unavailable)?;
    let descriptor = match openat(
        &directory,
        STAGED_NAME,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(_) => return Err(EngineStateError::InvalidState),
    };
    let file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|_| EngineStateError::InvalidState)?;
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.len() > MAX_RECORD
    {
        return Err(EngineStateError::InvalidState);
    }
    let staged = StagedRecord {
        directory,
        file,
        name: STAGED_NAME.to_owned(),
        identity: (metadata.dev(), metadata.ino()),
        promoted: true,
    };
    if !staged.is_current() {
        return Err(EngineStateError::InvalidState);
    }
    installation
        .revalidate()
        .map_err(|_| EngineStateError::Unavailable)?;
    unlinkat(&staged.directory, STAGED_NAME, AtFlags::empty())
        .and_then(|()| fsync(&staged.directory))
        .map_err(|_| EngineStateError::PersistenceUncertain)
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
