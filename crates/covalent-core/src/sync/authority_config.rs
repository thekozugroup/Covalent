//! Immutable, protected authority pins for a local folder installation.
//!
//! Creation requires an explicit trust decision by the pairing/setup workflow.
//! This module does not authenticate invitations or grant membership. A runtime
//! must open this record before replaying events, and must never substitute a
//! peer's key or recreate missing trust after any child state exists.

use std::fmt;

use covalent_protocol::DeviceId;
use ed25519_dalek::VerifyingKey;
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{CoreError, KeyProtector, WrappedSecret};

use super::ids::{FolderId, WriterId};
use super::installation::SyncInstallationParts;
use super::machine::{FolderMachineConfig, FolderMachineLimits};
use super::membership::{MemberGrant, MemberRole};
use super::state_dir::{
    PrivateStateEntryKind, PrivateStateFile, PrivateStateInventoryName, StateDirError, StateKey,
};

const RECORD_KEY: &str = "authority.v1";
const RECORD_MAGIC: &[u8; 8] = b"COVSAUT1";
const PLAINTEXT_MAGIC: &[u8; 8] = b"COVSAPN1";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 8 + 2 + 1 + 4;
const PLAINTEXT_BYTES: usize = 8 + 2 + 16 + 32 + 16 + 32;
const PURPOSE: &str = "covalent/sync-authority/v1";
const CONTEXT_DOMAIN: &[u8] = b"covalent/sync-authority-context/v1\0";
const CONTEXT_BYTES: usize = CONTEXT_DOMAIN.len() + 16 * 4 + 32;

/// Maximum complete protected record before JSON decoding or authentication.
pub const MAX_AUTHORITY_RECORD_BYTES: usize = 16 * 1024;

/// Fixed failures never expose key bytes, paths or untrusted record contents.
#[derive(Debug, Error)]
pub enum AuthorityConfigError {
    #[error("folder authority configuration is invalid")]
    InvalidConfiguration,
    #[error("folder authority requires an installation without child state")]
    DirectoryNotFresh,
    #[error("folder authority record is invalid")]
    InvalidRecord,
    #[error("folder authority record could not be authenticated")]
    Authentication,
    #[error("local transport identity differs from the installed folder binding")]
    WrongTransport,
    #[error("secure local randomness is unavailable")]
    EntropyUnavailable,
    #[error(transparent)]
    State(#[from] StateDirError),
}

/// Public authority identity selected by a separately authenticated setup flow.
/// Structural validity is not evidence that a peer is authorized.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PinnedFolderAuthority {
    writer: WriterId,
    key: VerifyingKey,
}

impl PinnedFolderAuthority {
    pub fn new(writer: WriterId, key: VerifyingKey) -> Result<Self, AuthorityConfigError> {
        if writer.to_bytes() == [0; 16] || key.is_weak() {
            return Err(AuthorityConfigError::InvalidConfiguration);
        }
        Ok(Self { writer, key })
    }

    #[must_use]
    pub const fn writer_id(self) -> WriterId {
        self.writer
    }

    #[must_use]
    pub const fn verifying_key(self) -> VerifyingKey {
        self.key
    }
}

impl fmt::Debug for PinnedFolderAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedFolderAuthority")
            .field("identity", &"[REDACTED]")
            .finish()
    }
}

/// Opaque stable commitment to authenticated setup identities. A future ready
/// record compares this commitment with freshly opened authority configuration;
/// the commitment alone is not a membership or filesystem capability.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct FolderSetupBinding([u8; 32]);

impl FolderSetupBinding {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for FolderSetupBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FolderSetupBinding([REDACTED])")
    }
}

/// Authenticated immutable pins, bound to the exact protected installation.
/// This value has no filesystem or membership-grant authority of its own.
pub struct FolderAuthorityConfig {
    folder: FolderId,
    local_writer: WriterId,
    authority: PinnedFolderAuthority,
    transport_device: DeviceId,
    transport_key: VerifyingKey,
    setup_binding: FolderSetupBinding,
}

impl fmt::Debug for FolderAuthorityConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FolderAuthorityConfig")
            .field("identities", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl FolderAuthorityConfig {
    /// Persists a setup workflow's explicit authority and local transport pins.
    ///
    /// Only installation.v1 and the held writer.lock may precede creation.
    /// Exclusive creation never overwrites even an incomplete incumbent. After
    /// an uncertain write, open the exact record; do not retry by replacing it.
    pub fn create(
        installation: &SyncInstallationParts,
        authority: PinnedFolderAuthority,
        transport_device: DeviceId,
        transport_key: VerifyingKey,
        protector: &dyn KeyProtector,
    ) -> Result<Self, AuthorityConfigError> {
        let config = Self::validated(installation, authority, transport_device, transport_key)?;
        require_pristine_installation(installation)?;
        let record = encode(installation, &config, protector)?;
        let file = installation.directory.create_new_file(
            &installation.lock,
            &StateKey::new(RECORD_KEY)?,
            &record,
            MAX_AUTHORITY_RECORD_BYTES as u64,
        )?;
        finish(installation, file, protector)
    }

    /// Opens the installed pins without accepting a replacement from a peer.
    pub fn open(
        installation: &SyncInstallationParts,
        protector: &dyn KeyProtector,
    ) -> Result<Self, AuthorityConfigError> {
        installation.directory.sync(&installation.lock)?;
        let file = installation.directory.open_file(
            &StateKey::new(RECORD_KEY)?,
            MAX_AUTHORITY_RECORD_BYTES as u64,
        )?;
        finish(installation, file, protector)
    }

    #[must_use]
    pub const fn authority(&self) -> PinnedFolderAuthority {
        self.authority
    }

    /// Stable commitment computed from the authenticated setup, including its
    /// installation generation. Callers cannot supply replacement digest bytes.
    #[must_use]
    pub const fn setup_binding(&self) -> FolderSetupBinding {
        self.setup_binding
    }

    /// Creates replay configuration from durable pins, never from event claims.
    #[must_use]
    pub fn machine_config(&self, limits: FolderMachineLimits) -> FolderMachineConfig {
        FolderMachineConfig {
            folder_id: self.folder,
            authority_writer_id: self.authority.writer,
            pinned_authority_key: self.authority.key,
            local_writer_id: self.local_writer,
            limits,
        }
    }

    /// Requires the live node's protected transport identity to match setup.
    pub fn require_local_transport(
        &self,
        device: DeviceId,
        key: VerifyingKey,
    ) -> Result<(), AuthorityConfigError> {
        if self.transport_device != device || self.transport_key != key {
            return Err(AuthorityConfigError::WrongTransport);
        }
        Ok(())
    }

    fn validated(
        installation: &SyncInstallationParts,
        authority: PinnedFolderAuthority,
        transport_device: DeviceId,
        transport_key: VerifyingKey,
    ) -> Result<Self, AuthorityConfigError> {
        let local_key = installation.signing_key.verifying_key();
        let same_writer = authority.writer == installation.writer_id;
        let same_key = authority.key == local_key;
        if installation.folder_id.to_bytes() == [0; 16]
            || same_writer != same_key
            || device_uuid(transport_device)?.is_nil()
            || authority.key == transport_key
        {
            return Err(AuthorityConfigError::InvalidConfiguration);
        }
        PinnedFolderAuthority::new(authority.writer, authority.key)?;
        MemberGrant::new(
            installation.writer_id,
            local_key,
            transport_device,
            transport_key,
            MemberRole::ReadWrite,
        )
        .map_err(|_| AuthorityConfigError::InvalidConfiguration)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"covalent/sync-folder-setup-binding/v1\0");
        hasher.update(&context(installation));
        hasher.update(&authority.writer.to_bytes());
        hasher.update(&authority.key.to_bytes());
        hasher.update(device_uuid(transport_device)?.as_bytes());
        hasher.update(&transport_key.to_bytes());
        Ok(Self {
            folder: installation.folder_id,
            local_writer: installation.writer_id,
            authority,
            transport_device,
            transport_key,
            setup_binding: FolderSetupBinding(*hasher.finalize().as_bytes()),
        })
    }
}

fn require_pristine_installation(
    installation: &SyncInstallationParts,
) -> Result<(), AuthorityConfigError> {
    let inventory = installation
        .directory
        .inventory(&installation.lock, 3, 192)?;
    let has_installation = inventory.entries().iter().any(|entry| {
        entry.kind() == PrivateStateEntryKind::RegularFile
            && entry
                .name()
                .state_key()
                .is_some_and(|key| key.as_str() == "installation.v1")
    });
    let has_lock = inventory.entries().iter().any(|entry| {
        entry.kind() == PrivateStateEntryKind::WriterLock
            && matches!(entry.name(), PrivateStateInventoryName::WriterLock)
    });
    if inventory.entries().len() != 2 || !has_installation || !has_lock {
        return Err(AuthorityConfigError::DirectoryNotFresh);
    }
    Ok(())
}

fn context(installation: &SyncInstallationParts) -> [u8; CONTEXT_BYTES] {
    let mut bytes = [0; CONTEXT_BYTES];
    let mut offset = 0;
    for part in [
        CONTEXT_DOMAIN,
        &installation.folder_id.to_bytes(),
        installation.installation_id.as_bytes(),
        installation.generation_id.as_bytes(),
        &installation.writer_id.to_bytes(),
        &installation.signing_key.verifying_key().to_bytes(),
    ] {
        bytes[offset..offset + part.len()].copy_from_slice(part);
        offset += part.len();
    }
    bytes
}

fn encode(
    installation: &SyncInstallationParts,
    config: &FolderAuthorityConfig,
    protector: &dyn KeyProtector,
) -> Result<Zeroizing<Vec<u8>>, AuthorityConfigError> {
    let mut plaintext = Zeroizing::new(Vec::with_capacity(PLAINTEXT_BYTES));
    plaintext.extend_from_slice(PLAINTEXT_MAGIC);
    plaintext.extend_from_slice(&VERSION.to_be_bytes());
    plaintext.extend_from_slice(&config.authority.writer.to_bytes());
    plaintext.extend_from_slice(&config.authority.key.to_bytes());
    plaintext.extend_from_slice(device_uuid(config.transport_device)?.as_bytes());
    plaintext.extend_from_slice(&config.transport_key.to_bytes());
    let envelope = WrappedSecret::protect(protector, PURPOSE, &context(installation), plaintext)
        .map_err(|error| match error {
            CoreError::EntropyUnavailable => AuthorityConfigError::EntropyUnavailable,
            _ => AuthorityConfigError::Authentication,
        })?;
    let envelope =
        serde_json::to_vec(&envelope).map_err(|_| AuthorityConfigError::InvalidRecord)?;
    if envelope.len() > MAX_AUTHORITY_RECORD_BYTES - HEADER_BYTES {
        return Err(AuthorityConfigError::InvalidRecord);
    }
    let mut record = Zeroizing::new(Vec::with_capacity(HEADER_BYTES + envelope.len()));
    record.extend_from_slice(RECORD_MAGIC);
    record.extend_from_slice(&VERSION.to_be_bytes());
    record.push(0);
    record.extend_from_slice(&(envelope.len() as u32).to_be_bytes());
    record.extend_from_slice(&envelope);
    Ok(record)
}

fn finish(
    installation: &SyncInstallationParts,
    file: PrivateStateFile,
    protector: &dyn KeyProtector,
) -> Result<FolderAuthorityConfig, AuthorityConfigError> {
    let bytes = file.read_all()?;
    let config = decode(installation, &bytes, protector)?;
    file.sync_all(&installation.lock)?;
    installation.directory.sync(&installation.lock)?;
    Ok(config)
}

fn decode(
    installation: &SyncInstallationParts,
    bytes: &[u8],
    protector: &dyn KeyProtector,
) -> Result<FolderAuthorityConfig, AuthorityConfigError> {
    if !(HEADER_BYTES..=MAX_AUTHORITY_RECORD_BYTES).contains(&bytes.len())
        || &bytes[..8] != RECORD_MAGIC
        || bytes[8..10] != VERSION.to_be_bytes()
        || bytes[10] != 0
    {
        return Err(AuthorityConfigError::InvalidRecord);
    }
    let length = u32::from_be_bytes(
        bytes[11..15]
            .try_into()
            .map_err(|_| AuthorityConfigError::InvalidRecord)?,
    ) as usize;
    if length == 0 || length != bytes.len() - HEADER_BYTES {
        return Err(AuthorityConfigError::InvalidRecord);
    }
    let payload = &bytes[HEADER_BYTES..];
    let envelope: WrappedSecret =
        serde_json::from_slice(payload).map_err(|_| AuthorityConfigError::InvalidRecord)?;
    if serde_json::to_vec(&envelope).map_err(|_| AuthorityConfigError::InvalidRecord)? != payload {
        return Err(AuthorityConfigError::InvalidRecord);
    }
    let plaintext = envelope
        .open(protector, PURPOSE, &context(installation))
        .map_err(|_| AuthorityConfigError::Authentication)?;
    if plaintext.len() != PLAINTEXT_BYTES
        || &plaintext[..8] != PLAINTEXT_MAGIC
        || plaintext[8..10] != VERSION.to_be_bytes()
    {
        return Err(AuthorityConfigError::InvalidRecord);
    }
    let authority = PinnedFolderAuthority::new(
        WriterId::from_uuid(
            Uuid::from_slice(&plaintext[10..26])
                .map_err(|_| AuthorityConfigError::InvalidRecord)?,
        ),
        public_key(&plaintext[26..58])?,
    )?;
    let device = DeviceId::from_uuid(
        Uuid::from_slice(&plaintext[58..74]).map_err(|_| AuthorityConfigError::InvalidRecord)?,
    );
    FolderAuthorityConfig::validated(
        installation,
        authority,
        device,
        public_key(&plaintext[74..106])?,
    )
}

fn public_key(bytes: &[u8]) -> Result<VerifyingKey, AuthorityConfigError> {
    VerifyingKey::from_bytes(
        bytes
            .try_into()
            .map_err(|_| AuthorityConfigError::InvalidRecord)?,
    )
    .map_err(|_| AuthorityConfigError::InvalidRecord)
}

fn device_uuid(device: DeviceId) -> Result<Uuid, AuthorityConfigError> {
    Uuid::parse_str(&device.to_string()).map_err(|_| AuthorityConfigError::InvalidConfiguration)
}

#[cfg(test)]
mod tests;
