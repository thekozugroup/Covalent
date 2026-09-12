//! Protected immutable local writer/key installation for one sync folder.
//!
//! This module owns only a fresh private-state root's `installation.v1` record.
//! It neither authorizes a folder member nor imports owner recovery identities.
//! A future coordinator must retain the returned root lock, acquire children in
//! its documented order, and separately persist authority configuration.

use std::fmt;

use ed25519_dalek::SigningKey;
use rand_core::{OsRng, RngCore as _};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::{CoreError, KeyProtector, WrappedSecret};

use super::content_crypto::{ContentCryptoError, SyncContentCrypto, SyncContentDomain};
use super::ids::{FolderId, WriterId};
use super::log_frame::LogFrameKey;
use super::state_dir::{
    PrivateStateDir, PrivateStateEntryKind, PrivateStateFile, PrivateStateInventoryName,
    PrivateStateLock, StateDirError, StateKey,
};

const RECORD_KEY: &str = "installation.v1";
const RECORD_MAGIC: &[u8; 8] = b"COVSINS1";
const RECORD_VERSION: u16 = 1;
const RECORD_FLAGS: u8 = 0;
const RECORD_HEADER_BYTES: usize = 8 + 2 + 1 + (16 * 4) + 4;
const PLAINTEXT_MAGIC: &[u8; 8] = b"COVSIP01";
const PLAINTEXT_VERSION: u16 = 1;
const PLAINTEXT_BYTES: usize = 8 + 2 + (16 * 4) + (32 * 4);
const ENVELOPE_PURPOSE: &str = "covalent/sync-installation/v1";
const CONTEXT_DOMAIN: &[u8] = b"covalent/sync-installation-context/v1\0";
const CONTEXT_BYTES: usize = CONTEXT_DOMAIN.len() + (16 * 4);

/// Hard limit for one protected sync installation record before JSON decoding.
pub const MAX_SYNC_INSTALLATION_RECORD_BYTES: usize = 16 * 1024;

/// Fixed failures from local installation creation or opening.
#[derive(Debug, Error)]
pub enum InstallationError {
    /// The supplied root contains something other than its held writer lock.
    #[error("sync installation requires a fresh private state directory")]
    DirectoryNotFresh,
    /// Platform randomness could not produce independent local secrets.
    #[error("secure local randomness is unavailable")]
    EntropyUnavailable,
    /// The bounded installation record was malformed or noncanonical.
    #[error("sync installation record is invalid")]
    InvalidRecord,
    /// The supplied folder was not the authenticated installation folder.
    #[error("sync installation belongs to a different folder")]
    WrongFolder,
    /// Key protection did not authenticate the installation record.
    #[error("sync installation could not be authenticated")]
    Authentication,
    /// A protected secret could not initialize its local content domain.
    #[error("sync installation key material is invalid")]
    InvalidKeyMaterial,
    /// Descriptor-anchored private state rejected this operation.
    #[error(transparent)]
    State(#[from] StateDirError),
}

/// A newly created or authenticated immutable local installation.
///
/// The root lock remains held for this handle's entire lifetime. Call
/// [`Self::into_parts`] only when handing the locked root to a coordinator;
/// dropping the returned lock before the coordinator has established its child
/// transaction ordering forfeits that serialization guarantee.
pub struct SyncInstallation {
    directory: PrivateStateDir,
    lock: PrivateStateLock,
    folder_id: FolderId,
    installation_id: Uuid,
    generation_id: Uuid,
    writer_id: WriterId,
    signing_seed: Zeroizing<[u8; 32]>,
    event_log_key: Zeroizing<[u8; 32]>,
    apply_log_key: Zeroizing<[u8; 32]>,
    content_generation_secret: Zeroizing<[u8; 32]>,
}

impl fmt::Debug for SyncInstallation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyncInstallation")
            .field("folder_id", &self.folder_id)
            .field("installation_id", &self.installation_id)
            .field("generation_id", &self.generation_id)
            .field("writer_id", &self.writer_id)
            .field("secrets", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl SyncInstallation {
    /// Creates the one immutable protected installation in a fresh root.
    ///
    /// This consumes the anchored root and retains its writer lock. If an
    /// exclusive write may have reached disk but does not report success, the
    /// caller must use [`Self::open`] to inspect that exact incumbent; create
    /// never replaces or recreates it.
    pub fn create(
        directory: PrivateStateDir,
        folder_id: FolderId,
        protector: &dyn KeyProtector,
    ) -> Result<Self, InstallationError> {
        let lock = directory.try_lock()?;
        require_fresh_directory(&directory, &lock)?;
        let material = InstallationMaterial::generate()?;
        let record = encode_record(folder_id, material, protector)?;
        let record_key = StateKey::new(RECORD_KEY).map_err(|_| InstallationError::InvalidRecord)?;
        let file = directory.create_new_file(
            &lock,
            &record_key,
            record.as_ref(),
            MAX_SYNC_INSTALLATION_RECORD_BYTES as u64,
        )?;
        finish_from_file(directory, lock, file, folder_id, protector)
    }

    /// Opens the exact protected installation for `expected_folder`.
    ///
    /// Other coordinator child state may coexist with `installation.v1`; this
    /// method authenticates only that immutable record while retaining the root
    /// writer lock for the caller's later coordinator handoff.
    pub fn open(
        directory: PrivateStateDir,
        expected_folder: FolderId,
        protector: &dyn KeyProtector,
    ) -> Result<Self, InstallationError> {
        let lock = directory.try_lock()?;
        let record_key = StateKey::new(RECORD_KEY).map_err(|_| InstallationError::InvalidRecord)?;
        let file = directory.open_file(&record_key, MAX_SYNC_INSTALLATION_RECORD_BYTES as u64)?;
        finish_from_file(directory, lock, file, expected_folder, protector)
    }

    /// Transfers the held root lock, identities, and configured local keys.
    ///
    /// The receiving coordinator must acquire child locks in its documented
    /// fixed order and must exclude `installation.v1` from any owner-recovery
    /// export. This is not an authority or runtime-completion claim.
    pub fn into_parts(self) -> Result<SyncInstallationParts, InstallationError> {
        let Self {
            directory,
            lock,
            folder_id,
            installation_id,
            generation_id,
            writer_id,
            mut signing_seed,
            mut event_log_key,
            mut apply_log_key,
            mut content_generation_secret,
        } = self;
        let signing_key = SigningKey::from_bytes(&signing_seed);
        signing_seed.zeroize();
        let mut event_key_bytes = *event_log_key;
        event_log_key.zeroize();
        let event_log_key = LogFrameKey::from_bytes(event_key_bytes);
        event_key_bytes.zeroize();
        let mut apply_key_bytes = *apply_log_key;
        apply_log_key.zeroize();
        let apply_log_key = LogFrameKey::from_bytes(apply_key_bytes);
        apply_key_bytes.zeroize();
        let mut content_key_bytes = *content_generation_secret;
        content_generation_secret.zeroize();
        let content_crypto = SyncContentCrypto::from_bytes(
            SyncContentDomain::new(folder_id, installation_id, generation_id),
            content_key_bytes,
        );
        content_key_bytes.zeroize();
        let content_crypto = content_crypto.map_err(map_content_error)?;
        Ok(SyncInstallationParts {
            directory,
            lock,
            folder_id,
            installation_id,
            generation_id,
            writer_id,
            signing_key,
            event_log_key,
            apply_log_key,
            content_crypto,
        })
    }

    /// Returns the folder bound to this protected record.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the fresh local installation UUID.
    #[must_use]
    pub const fn installation_id(&self) -> Uuid {
        self.installation_id
    }

    /// Returns the independent protected-key generation UUID.
    #[must_use]
    pub const fn generation_id(&self) -> Uuid {
        self.generation_id
    }

    /// Returns this folder installation's distinct operation writer identity.
    #[must_use]
    pub const fn writer_id(&self) -> WriterId {
        self.writer_id
    }
}

/// A consuming coordinator handoff that retains the exclusive root lock.
///
/// There is deliberately no `Clone`, `Debug`, secret-byte accessor, recovery
/// export, or authority pinning API. The core coordinator may consume the
/// crate-visible fields to transfer keys into their owning log/store handles.
/// It must retain the root lock throughout that handoff and subsequent work.
pub struct SyncInstallationParts {
    pub(crate) directory: PrivateStateDir,
    pub(crate) lock: PrivateStateLock,
    pub(crate) folder_id: FolderId,
    pub(crate) installation_id: Uuid,
    pub(crate) generation_id: Uuid,
    pub(crate) writer_id: WriterId,
    pub(crate) signing_key: SigningKey,
    pub(crate) event_log_key: LogFrameKey,
    pub(crate) apply_log_key: LogFrameKey,
    pub(crate) content_crypto: SyncContentCrypto,
}

impl SyncInstallationParts {
    /// Borrows the anchored root while its exclusive installation lock is held.
    #[must_use]
    pub fn directory(&self) -> &PrivateStateDir {
        &self.directory
    }

    /// Borrows the root lock that a coordinator must retain before children.
    #[must_use]
    pub fn root_lock(&self) -> &PrivateStateLock {
        &self.lock
    }

    /// Returns the folder installation's identities and configured keys.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the local installation UUID.
    #[must_use]
    pub const fn installation_id(&self) -> Uuid {
        self.installation_id
    }

    /// Returns the local protected-key generation UUID.
    #[must_use]
    pub const fn generation_id(&self) -> Uuid {
        self.generation_id
    }

    /// Returns the folder-global local writer identity.
    #[must_use]
    pub const fn writer_id(&self) -> WriterId {
        self.writer_id
    }

    /// Borrows the configured signing key for a coordinator-owned log.
    #[must_use]
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Borrows the local folder-event log key.
    #[must_use]
    pub fn event_log_key(&self) -> &LogFrameKey {
        &self.event_log_key
    }

    /// Borrows the independent local apply-log key.
    #[must_use]
    pub fn apply_log_key(&self) -> &LogFrameKey {
        &self.apply_log_key
    }

    /// Borrows the generation-bound local content cryptography.
    #[must_use]
    pub fn content_crypto(&self) -> &SyncContentCrypto {
        &self.content_crypto
    }
}

struct InstallationMaterial {
    installation_id: Uuid,
    generation_id: Uuid,
    writer_id: WriterId,
    signing_seed: Zeroizing<[u8; 32]>,
    event_log_key: Zeroizing<[u8; 32]>,
    apply_log_key: Zeroizing<[u8; 32]>,
    content_generation_secret: Zeroizing<[u8; 32]>,
}

impl InstallationMaterial {
    fn generate() -> Result<Self, InstallationError> {
        let installation_id = random_uuid()?;
        let generation_id = random_uuid()?;
        let writer_id = WriterId::from_uuid(random_uuid()?);
        Ok(Self {
            installation_id,
            generation_id,
            writer_id,
            signing_seed: random_secret()?,
            event_log_key: random_secret()?,
            apply_log_key: random_secret()?,
            content_generation_secret: random_secret()?,
        })
    }
}

fn finish_from_file(
    directory: PrivateStateDir,
    lock: PrivateStateLock,
    file: PrivateStateFile,
    expected_folder: FolderId,
    protector: &dyn KeyProtector,
) -> Result<SyncInstallation, InstallationError> {
    let bytes = file.read_all()?;
    let material = decode_record(expected_folder, protector, &bytes)?;
    // A newly created incumbent might have survived file sync without its
    // parent insertion. Do not return usable secrets until both are confirmed.
    file.sync_all(&lock)?;
    directory.sync(&lock)?;
    Ok(SyncInstallation {
        directory,
        lock,
        folder_id: expected_folder,
        installation_id: material.installation_id,
        generation_id: material.generation_id,
        writer_id: material.writer_id,
        signing_seed: material.signing_seed,
        event_log_key: material.event_log_key,
        apply_log_key: material.apply_log_key,
        content_generation_secret: material.content_generation_secret,
    })
}

fn require_fresh_directory(
    directory: &PrivateStateDir,
    lock: &PrivateStateLock,
) -> Result<(), InstallationError> {
    let inventory = directory.inventory(lock, 2, 128)?;
    if inventory.entries().len() != 1
        || !matches!(
            inventory.entries().first(),
            Some(entry)
                if matches!(entry.name(), PrivateStateInventoryName::WriterLock)
                    && entry.kind() == PrivateStateEntryKind::WriterLock
        )
    {
        return Err(InstallationError::DirectoryNotFresh);
    }
    Ok(())
}

fn encode_record(
    folder_id: FolderId,
    material: InstallationMaterial,
    protector: &dyn KeyProtector,
) -> Result<Zeroizing<Vec<u8>>, InstallationError> {
    let mut plaintext = Zeroizing::new(Vec::new());
    plaintext
        .try_reserve_exact(PLAINTEXT_BYTES)
        .map_err(|_| InstallationError::InvalidRecord)?;
    plaintext.extend_from_slice(PLAINTEXT_MAGIC);
    plaintext.extend_from_slice(&PLAINTEXT_VERSION.to_be_bytes());
    plaintext.extend_from_slice(&folder_id.to_bytes());
    plaintext.extend_from_slice(material.installation_id.as_bytes());
    plaintext.extend_from_slice(material.generation_id.as_bytes());
    plaintext.extend_from_slice(&material.writer_id.to_bytes());
    plaintext.extend_from_slice(material.signing_seed.as_ref());
    plaintext.extend_from_slice(material.event_log_key.as_ref());
    plaintext.extend_from_slice(material.apply_log_key.as_ref());
    plaintext.extend_from_slice(material.content_generation_secret.as_ref());
    debug_assert_eq!(plaintext.len(), PLAINTEXT_BYTES);

    let context = installation_context(
        folder_id,
        material.installation_id,
        material.generation_id,
        material.writer_id,
    );
    let envelope = WrappedSecret::protect(protector, ENVELOPE_PURPOSE, &context, plaintext)
        .map_err(|error| match error {
            CoreError::EntropyUnavailable => InstallationError::EntropyUnavailable,
            _ => InstallationError::Authentication,
        })?;
    let envelope = serde_json::to_vec(&envelope).map_err(|_| InstallationError::InvalidRecord)?;
    let total = RECORD_HEADER_BYTES
        .checked_add(envelope.len())
        .filter(|length| *length <= MAX_SYNC_INSTALLATION_RECORD_BYTES)
        .ok_or(InstallationError::InvalidRecord)?;
    let envelope_length =
        u32::try_from(envelope.len()).map_err(|_| InstallationError::InvalidRecord)?;
    let mut record = Zeroizing::new(Vec::new());
    record
        .try_reserve_exact(total)
        .map_err(|_| InstallationError::InvalidRecord)?;
    record.extend_from_slice(RECORD_MAGIC);
    record.extend_from_slice(&RECORD_VERSION.to_be_bytes());
    record.push(RECORD_FLAGS);
    record.extend_from_slice(&folder_id.to_bytes());
    record.extend_from_slice(material.installation_id.as_bytes());
    record.extend_from_slice(material.generation_id.as_bytes());
    record.extend_from_slice(&material.writer_id.to_bytes());
    record.extend_from_slice(&envelope_length.to_be_bytes());
    record.extend_from_slice(&envelope);
    debug_assert_eq!(record.len(), total);
    Ok(record)
}

fn decode_record(
    expected_folder: FolderId,
    protector: &dyn KeyProtector,
    bytes: &[u8],
) -> Result<InstallationMaterial, InstallationError> {
    let header = decode_header(bytes)?;
    if header.folder_id != expected_folder {
        return Err(InstallationError::WrongFolder);
    }
    let envelope: WrappedSecret =
        serde_json::from_slice(header.envelope).map_err(|_| InstallationError::InvalidRecord)?;
    let canonical = serde_json::to_vec(&envelope).map_err(|_| InstallationError::InvalidRecord)?;
    if canonical != header.envelope {
        return Err(InstallationError::InvalidRecord);
    }
    let context = installation_context(
        header.folder_id,
        header.installation_id,
        header.generation_id,
        header.writer_id,
    );
    let plaintext = envelope
        .open(protector, ENVELOPE_PURPOSE, &context)
        .map_err(|_| InstallationError::Authentication)?;
    decode_plaintext(header, &plaintext)
}

struct RecordHeader<'a> {
    folder_id: FolderId,
    installation_id: Uuid,
    generation_id: Uuid,
    writer_id: WriterId,
    envelope: &'a [u8],
}

fn decode_header(bytes: &[u8]) -> Result<RecordHeader<'_>, InstallationError> {
    if bytes.len() > MAX_SYNC_INSTALLATION_RECORD_BYTES || bytes.len() < RECORD_HEADER_BYTES {
        return Err(InstallationError::InvalidRecord);
    }
    let mut cursor = Cursor::new(bytes);
    if cursor.take(8)? != RECORD_MAGIC
        || cursor.u16()? != RECORD_VERSION
        || cursor.u8()? != RECORD_FLAGS
    {
        return Err(InstallationError::InvalidRecord);
    }
    let folder_id = FolderId::from_uuid(cursor.uuid()?);
    let installation_id = cursor.uuid()?;
    let generation_id = cursor.uuid()?;
    let writer_id = WriterId::from_uuid(cursor.uuid()?);
    let envelope_length =
        usize::try_from(cursor.u32()?).map_err(|_| InstallationError::InvalidRecord)?;
    if envelope_length == 0 || cursor.remaining() != envelope_length {
        return Err(InstallationError::InvalidRecord);
    }
    Ok(RecordHeader {
        folder_id,
        installation_id,
        generation_id,
        writer_id,
        envelope: cursor.take(envelope_length)?,
    })
}

fn decode_plaintext(
    header: RecordHeader<'_>,
    plaintext: &[u8],
) -> Result<InstallationMaterial, InstallationError> {
    if plaintext.len() != PLAINTEXT_BYTES {
        return Err(InstallationError::InvalidRecord);
    }
    let mut cursor = Cursor::new(plaintext);
    if cursor.take(8)? != PLAINTEXT_MAGIC || cursor.u16()? != PLAINTEXT_VERSION {
        return Err(InstallationError::InvalidRecord);
    }
    let folder_id = FolderId::from_uuid(cursor.uuid()?);
    let installation_id = cursor.uuid()?;
    let generation_id = cursor.uuid()?;
    let writer_id = WriterId::from_uuid(cursor.uuid()?);
    if folder_id != header.folder_id
        || installation_id != header.installation_id
        || generation_id != header.generation_id
        || writer_id != header.writer_id
    {
        return Err(InstallationError::Authentication);
    }
    let signing_seed = cursor.secret()?;
    let event_log_key = cursor.secret()?;
    let apply_log_key = cursor.secret()?;
    let content_generation_secret = cursor.secret()?;
    if cursor.remaining() != 0 {
        return Err(InstallationError::InvalidRecord);
    }
    Ok(InstallationMaterial {
        installation_id,
        generation_id,
        writer_id,
        signing_seed,
        event_log_key,
        apply_log_key,
        content_generation_secret,
    })
}

fn installation_context(
    folder_id: FolderId,
    installation_id: Uuid,
    generation_id: Uuid,
    writer_id: WriterId,
) -> [u8; CONTEXT_BYTES] {
    let mut context = [0_u8; CONTEXT_BYTES];
    let mut offset = 0;
    context[..CONTEXT_DOMAIN.len()].copy_from_slice(CONTEXT_DOMAIN);
    offset += CONTEXT_DOMAIN.len();
    for value in [
        folder_id.to_bytes(),
        *installation_id.as_bytes(),
        *generation_id.as_bytes(),
        writer_id.to_bytes(),
    ] {
        context[offset..offset + value.len()].copy_from_slice(&value);
        offset += value.len();
    }
    context
}

fn random_uuid() -> Result<Uuid, InstallationError> {
    let mut bytes = [0_u8; 16];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| InstallationError::EntropyUnavailable)?;
    // Allocate RFC 4122 v4 identifiers from the same fallible OS entropy
    // source as all local secret material.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Uuid::from_bytes(bytes))
}

fn random_secret() -> Result<Zeroizing<[u8; 32]>, InstallationError> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    OsRng
        .try_fill_bytes(bytes.as_mut())
        .map_err(|_| InstallationError::EntropyUnavailable)?;
    Ok(bytes)
}

fn map_content_error(_: ContentCryptoError) -> InstallationError {
    InstallationError::InvalidKeyMaterial
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], InstallationError> {
        let end = self
            .offset
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(InstallationError::InvalidRecord)?;
        let result = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(result)
    }

    fn u8(&mut self) -> Result<u8, InstallationError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, InstallationError> {
        self.take(2)?
            .try_into()
            .map(u16::from_be_bytes)
            .map_err(|_| InstallationError::InvalidRecord)
    }

    fn u32(&mut self) -> Result<u32, InstallationError> {
        self.take(4)?
            .try_into()
            .map(u32::from_be_bytes)
            .map_err(|_| InstallationError::InvalidRecord)
    }

    fn uuid(&mut self) -> Result<Uuid, InstallationError> {
        self.take(16)?
            .try_into()
            .map(Uuid::from_bytes)
            .map_err(|_| InstallationError::InvalidRecord)
    }

    fn secret(&mut self) -> Result<Zeroizing<[u8; 32]>, InstallationError> {
        let mut secret = Zeroizing::new([0_u8; 32]);
        secret.copy_from_slice(self.take(32)?);
        Ok(secret)
    }

    const fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::TempDir;

    use super::*;
    use crate::StaticKeyProtector;
    use crate::sync::body::{ContentDigest, FileContent};
    use crate::sync::content_manifest::{ChunkDescriptor, ChunkDigest};
    use crate::sync::log_frame::{
        LogBinding, LogFileKind, LogOrdinal, ParseOutcome, encode_frame, parse_one,
    };

    fn folder(number: u128) -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(number))
    }

    fn protector(byte: u8) -> StaticKeyProtector {
        StaticKeyProtector::new(1, [byte; 32]).expect("test protector")
    }

    fn private_root() -> (TempDir, PrivateStateDir) {
        let temporary = TempDir::new().expect("temporary private root");
        fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .expect("protect temporary private root");
        let directory = PrivateStateDir::open_root(temporary.path()).expect("open private root");
        (temporary, directory)
    }

    fn reopen(temporary: &TempDir) -> PrivateStateDir {
        PrivateStateDir::open_root(temporary.path()).expect("reopen private root")
    }

    #[test]
    fn protected_record_reopens_the_same_local_identities_and_keys() {
        let (temporary, directory) = private_root();
        let folder = folder(1);
        let created = SyncInstallation::create(directory, folder, &protector(7)).expect("create");
        assert_eq!(
            fs::metadata(temporary.path().join(RECORD_KEY))
                .expect("record metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let expected = (
            created.folder_id(),
            created.installation_id(),
            created.generation_id(),
            created.writer_id(),
        );
        let parts = created.into_parts().expect("configure created");
        let signing = parts.signing_key().verifying_key().to_bytes();
        let content =
            FileContent::new(ContentDigest::from_bytes([3; 32]), 9, false).expect("file content");
        let chunk = ChunkDescriptor::new(ChunkDigest::from_bytes([4; 32]), 5).expect("chunk");
        let manifest_locator = parts.content_crypto().manifest_locator(content).to_bytes();
        let chunk_locator = parts.content_crypto().chunk_locator(chunk).to_bytes();
        let binding = LogBinding::new(
            expected.0,
            expected.1,
            expected.2,
            LogFileKind::FolderEvents,
        );
        let frame = encode_frame(
            &binding,
            LogOrdinal::new(1).expect("ordinal"),
            parts.event_log_key(),
            b"same event key",
        )
        .expect("frame");
        let apply_binding = LogBinding::new(expected.0, expected.1, expected.2, LogFileKind::Apply);
        let apply_frame = encode_frame(
            &apply_binding,
            LogOrdinal::new(1).expect("ordinal"),
            parts.apply_log_key(),
            b"same apply key",
        )
        .expect("apply frame");
        drop(parts);

        let reopened =
            SyncInstallation::open(reopen(&temporary), folder, &protector(7)).expect("open");
        assert_eq!(
            (
                reopened.folder_id(),
                reopened.installation_id(),
                reopened.generation_id(),
                reopened.writer_id(),
            ),
            expected
        );
        let parts = reopened.into_parts().expect("configure reopened");
        assert_eq!(parts.signing_key().verifying_key().to_bytes(), signing);
        assert_eq!(
            parts.content_crypto().manifest_locator(content).to_bytes(),
            manifest_locator
        );
        assert_eq!(
            parts.content_crypto().chunk_locator(chunk).to_bytes(),
            chunk_locator
        );
        assert!(matches!(
            parse_one(
                frame.as_bytes(),
                &binding,
                LogOrdinal::new(1).expect("ordinal"),
                parts.event_log_key(),
            ),
            Ok(ParseOutcome::Complete(_))
        ));
        assert!(matches!(
            parse_one(
                apply_frame.as_bytes(),
                &apply_binding,
                LogOrdinal::new(1).expect("ordinal"),
                parts.apply_log_key(),
            ),
            Ok(ParseOutcome::Complete(_))
        ));
    }

    #[test]
    fn open_rejects_wrong_folder_protector_and_tampered_record() {
        let (temporary, directory) = private_root();
        let expected_folder = folder(2);
        let installation =
            SyncInstallation::create(directory, expected_folder, &protector(8)).expect("create");
        drop(installation);
        assert!(matches!(
            SyncInstallation::open(reopen(&temporary), folder(3), &protector(8)),
            Err(InstallationError::WrongFolder)
        ));
        assert!(matches!(
            SyncInstallation::open(reopen(&temporary), expected_folder, &protector(9)),
            Err(InstallationError::Authentication)
        ));
        let path = temporary.path().join(RECORD_KEY);
        let mut bytes = fs::read(&path).expect("read record");
        *bytes.last_mut().expect("record byte") ^= 1;
        fs::write(&path, bytes).expect("tamper record");
        assert!(matches!(
            SyncInstallation::open(reopen(&temporary), expected_folder, &protector(8)),
            Err(InstallationError::Authentication | InstallationError::InvalidRecord)
        ));
    }

    #[test]
    fn codec_rejects_every_prefix_trailing_and_oversize_before_use() {
        let folder = folder(4);
        let record = encode_record(folder, fixture_material(), &protector(10)).expect("record");
        for prefix in 0..record.len() {
            assert!(decode_header(&record[..prefix]).is_err(), "prefix {prefix}");
        }
        let mut trailing = record.to_vec();
        trailing.push(0);
        assert!(decode_header(&trailing).is_err());
        assert!(decode_header(&vec![0; MAX_SYNC_INSTALLATION_RECORD_BYTES + 1]).is_err());
    }

    #[test]
    fn create_never_replaces_an_incumbent_or_incomplete_record() {
        let (temporary, directory) = private_root();
        let lock = directory.try_lock().expect("lock");
        let key = StateKey::new(RECORD_KEY).expect("record key");
        directory
            .create_new_file(
                &lock,
                &key,
                b"incomplete",
                MAX_SYNC_INSTALLATION_RECORD_BYTES as u64,
            )
            .expect("write incumbent");
        drop(lock);
        assert!(matches!(
            SyncInstallation::create(reopen(&temporary), folder(5), &protector(11)),
            Err(InstallationError::DirectoryNotFresh)
        ));
        assert_eq!(
            fs::read(temporary.path().join(RECORD_KEY)).expect("incumbent"),
            b"incomplete"
        );
    }

    #[test]
    fn open_authenticates_the_installation_when_later_child_state_exists() {
        let (temporary, directory) = private_root();
        let folder = folder(51);
        let installation =
            SyncInstallation::create(directory, folder, &protector(11)).expect("create");
        drop(installation);
        let root = reopen(&temporary);
        let child = root
            .open_or_create_child(&StateKey::new("events").expect("child key"))
            .expect("later coordinator child");
        drop((root, child));
        assert!(SyncInstallation::open(reopen(&temporary), folder, &protector(11)).is_ok());
    }

    #[test]
    fn distinct_fresh_roots_allocate_distinct_writer_and_generation() {
        let (left_temporary, left_directory) = private_root();
        let (right_temporary, right_directory) = private_root();
        let left =
            SyncInstallation::create(left_directory, folder(6), &protector(12)).expect("left");
        let right =
            SyncInstallation::create(right_directory, folder(6), &protector(12)).expect("right");
        assert_ne!(left.writer_id(), right.writer_id());
        assert_ne!(left.generation_id(), right.generation_id());
        assert_ne!(left.installation_id(), right.installation_id());
        drop((left, right, left_temporary, right_temporary));
    }

    #[test]
    fn fresh_create_rejects_held_lock_and_nonprivate_or_symlink_roots() {
        let (temporary, directory) = private_root();
        let folder = folder(7);
        let installation =
            SyncInstallation::create(directory, folder, &protector(13)).expect("create");
        assert!(matches!(
            SyncInstallation::open(reopen(&temporary), folder, &protector(13)),
            Err(InstallationError::State(StateDirError::Locked))
        ));
        drop(installation);

        let unsafe_root = TempDir::new().expect("unsafe root");
        fs::set_permissions(
            unsafe_root.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("relax root");
        assert!(PrivateStateDir::open_root(unsafe_root.path()).is_err());

        let link_parent = TempDir::new().expect("link parent");
        let link = link_parent.path().join("root-link");
        std::os::unix::fs::symlink(temporary.path(), &link).expect("link");
        assert!(PrivateStateDir::open_root(&link).is_err());
    }

    #[test]
    fn encrypted_record_and_debug_do_not_expose_secret_canaries() {
        let folder = folder(8);
        let material = InstallationMaterial {
            installation_id: Uuid::from_u128(11),
            generation_id: Uuid::from_u128(12),
            writer_id: WriterId::from_uuid(Uuid::from_u128(13)),
            signing_seed: Zeroizing::new([0xa5; 32]),
            event_log_key: Zeroizing::new([0xa6; 32]),
            apply_log_key: Zeroizing::new([0xa7; 32]),
            content_generation_secret: Zeroizing::new([0xa8; 32]),
        };
        let record = encode_record(folder, material, &protector(14)).expect("encrypt record");
        assert!(!record.windows(32).any(|window| window == [0xa5; 32]));
        assert!(!record.windows(32).any(|window| window == [0xa6; 32]));
        assert!(!record.windows(32).any(|window| window == [0xa7; 32]));
        assert!(!record.windows(32).any(|window| window == [0xa8; 32]));

        let (temporary, directory) = private_root();
        let lock = directory.try_lock().expect("lock");
        let key = StateKey::new(RECORD_KEY).expect("record key");
        let file = directory
            .create_new_file(
                &lock,
                &key,
                &record,
                MAX_SYNC_INSTALLATION_RECORD_BYTES as u64,
            )
            .expect("persist encrypted canaries");
        let installation = finish_from_file(directory, lock, file, folder, &protector(14))
            .expect("open encrypted canaries");
        let debug = format!("{installation:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("165"));
        drop((installation, temporary));
    }

    fn fixture_material() -> InstallationMaterial {
        InstallationMaterial {
            installation_id: Uuid::from_u128(21),
            generation_id: Uuid::from_u128(22),
            writer_id: WriterId::from_uuid(Uuid::from_u128(23)),
            signing_seed: Zeroizing::new([1; 32]),
            event_log_key: Zeroizing::new([2; 32]),
            apply_log_key: Zeroizing::new([3; 32]),
            content_generation_secret: Zeroizing::new([4; 32]),
        }
    }
}
