//! Immutable, bounded local retention before sync operation publication.
//!
//! This first store keeps every final object and every abandoned staging file.
//! Reopening counts them all; reaching a quota stops ingestion visibly. There is
//! no eviction or implicit cleanup, so uncertain publication cannot lose its
//! content. A later explicit recovery/cleanup transaction may reclaim provably
//! unreferenced staging files. These limits count logical encrypted file bytes,
//! not filesystem allocation overhead or process RSS.

use std::fmt;
use std::io::{ErrorKind, Read};

use rand_core::{OsRng, RngCore as _};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::engine::JobControl;

use super::body::FileContent;
use super::content_crypto::{
    ContentLocator, EncodedContentRecord, MAX_ENCRYPTED_SYNC_CHUNK_BYTES,
    MAX_ENCRYPTED_SYNC_MANIFEST_BYTES, SyncContentCrypto, SyncContentDomain,
};
use super::content_manifest::{
    ChunkDescriptor, ChunkDigest, ContentManifest, MAX_SYNC_CONTENT_CHUNK_BYTES,
    MAX_SYNC_CONTENT_CHUNKS, MAX_SYNC_CONTENT_FILE_BYTES,
};
use super::state_dir::{
    PrivateStateDir, PrivateStateEntryKind, PrivateStateFile, PrivateStateLock,
    PrivateStatePromotion, StateDirError, StateKey,
};

const MAX_STORE_OBJECTS: u64 = 1_000_000;
const READ_STEP_BYTES: usize = 64 * 1_024;

/// Independent finite bounds, including abandoned and unreferenced objects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentStoreLimits {
    /// Maximum summed logical length of all encrypted object and staging files.
    pub maximum_stored_bytes: u64,
    /// Maximum content/staging files, at most one million. Fixed lock files and
    /// the three fixed private child directories are separate bounded overhead.
    pub maximum_objects: u64,
}

/// Complete accounted usage under the store's held cooperative writer locks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContentStoreUsage {
    /// Encrypted bytes, including incomplete or abandoned staging records.
    pub stored_bytes: u64,
    /// Final and staging content files; this includes unreferenced chunks.
    pub objects: u64,
}

/// Evidence that the store durably retained and verified one complete file.
///
/// This is not a peer acknowledgement or an apply receipt. It includes the exact
/// signed executable metadata even though encryption/cache identity excludes
/// that bit. The publication coordinator must match both the file reference and
/// the folder/installation/generation before using this receipt.
pub struct VerifiedContentReceipt {
    domain: SyncContentDomain,
    content: FileContent,
}

impl VerifiedContentReceipt {
    /// Returns the exact file reference verified before this receipt was issued.
    #[must_use]
    pub const fn content(&self) -> FileContent {
        self.content
    }

    /// Returns the private domain that durably holds these file bytes.
    #[must_use]
    pub const fn domain(&self) -> SyncContentDomain {
        self.domain
    }
}

impl fmt::Debug for VerifiedContentReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedContentReceipt")
            .field("byte_length", &self.content.byte_length())
            .finish_non_exhaustive()
    }
}

struct LockedDirectory {
    dir: PrivateStateDir,
    lock: PrivateStateLock,
}

impl LockedDirectory {
    fn new(dir: PrivateStateDir, create_missing: bool) -> Result<Self, ContentStoreError> {
        let lock = if create_missing {
            dir.try_lock()
        } else {
            dir.try_lock_existing()
        }
        .map_err(storage_error)?;
        Ok(Self { dir, lock })
    }
}

/// A single-owner immutable cache with lifetime exclusion of cooperative writers.
///
/// Hold the outer folder coordinator lock before opening this store. Internal
/// lock order is root, chunks, manifests, staging. Content reads reauthenticate
/// bytes each time; this does not make private files immune to external damage.
pub struct SyncContentStore {
    root: LockedDirectory,
    chunks: LockedDirectory,
    manifests: LockedDirectory,
    staging: LockedDirectory,
    crypto: SyncContentCrypto,
    limits: ContentStoreLimits,
    usage: ContentStoreUsage,
    poisoned: bool,
}

impl fmt::Debug for SyncContentStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyncContentStore")
            .field("usage", &self.usage)
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl SyncContentStore {
    /// Opens a private cache and counts every final and staging file before use.
    /// The caller has already durably created the root and protected the key.
    pub fn open(
        root: PrivateStateDir,
        crypto: SyncContentCrypto,
        limits: ContentStoreLimits,
    ) -> Result<Self, ContentStoreError> {
        Self::open_with_creation(root, crypto, limits, true)
    }

    /// Reopens complete existing storage without creating directories or locks.
    /// A missing child/lock is an error, preserving evidence for explicit
    /// initialization recovery rather than silently rebuilding ready state.
    pub fn open_existing(
        root: PrivateStateDir,
        crypto: SyncContentCrypto,
        limits: ContentStoreLimits,
    ) -> Result<Self, ContentStoreError> {
        Self::open_with_creation(root, crypto, limits, false)
    }

    fn open_with_creation(
        root: PrivateStateDir,
        crypto: SyncContentCrypto,
        limits: ContentStoreLimits,
        create_missing: bool,
    ) -> Result<Self, ContentStoreError> {
        if limits.maximum_stored_bytes == 0
            || limits.maximum_objects == 0
            || limits.maximum_objects > MAX_STORE_OBJECTS
        {
            return Err(ContentStoreError::InvalidLimits);
        }
        let root = LockedDirectory::new(root, create_missing)?;
        validate_root(&root)?;
        let child = |name| -> Result<LockedDirectory, ContentStoreError> {
            let key = internal_key(name)?;
            let directory = if create_missing {
                root.dir.open_or_create_child(&key)
            } else {
                root.dir.open_child(&key)
            }
            .map_err(storage_error)?;
            LockedDirectory::new(directory, create_missing)
        };
        let chunks = child("chunks")?;
        let manifests = child("manifests")?;
        let staging = child("staging")?;
        let mut store = Self {
            root,
            chunks,
            manifests,
            staging,
            crypto,
            limits,
            usage: ContentStoreUsage::default(),
            poisoned: false,
        };
        store.rescan()?;
        Ok(store)
    }

    /// Returns usage established by complete inventory and accounted writes.
    pub fn usage(&self) -> Result<ContentStoreUsage, ContentStoreError> {
        self.require_usable()?;
        Ok(self.usage)
    }

    /// Retains one verified bounded chunk without claiming a complete file.
    /// Callers authenticate and authorize network senders before supplying bytes.
    pub fn retain_chunk(
        &mut self,
        descriptor: ChunkDescriptor,
        plaintext: &[u8],
        control: &JobControl,
    ) -> Result<(), ContentStoreError> {
        self.require_usable()?;
        check_control(control)?;
        // Seal verifies digest and length even when an incumbent already exists.
        let record = self
            .crypto
            .seal_chunk(descriptor, plaintext)
            .map_err(|_| ContentStoreError::InvalidContent)?;
        self.persist(Object::Chunk(descriptor), &record)?;
        Ok(())
    }

    /// Verifies all ordered chunks and whole-file bytes, then commits the manifest.
    /// Every final object is reopened, authenticated and fsynced before success.
    pub fn retain_manifest(
        &mut self,
        expected: FileContent,
        manifest: &ContentManifest,
        control: &JobControl,
    ) -> Result<VerifiedContentReceipt, ContentStoreError> {
        self.require_usable()?;
        check_control(control)?;
        if manifest.whole_digest() != expected.digest()
            || manifest.byte_length() != expected.byte_length()
        {
            return Err(ContentStoreError::InvalidContent);
        }
        self.verify_chunks(expected, manifest, control)?;
        let record = self
            .crypto
            .seal_manifest(manifest)
            .map_err(|_| ContentStoreError::InvalidContent)?;
        self.persist(Object::Manifest(expected), &record)?;
        // An incumbent may use another valid chunking; verify its exact list too.
        self.verify_retained(expected, control)
    }

    /// Streams a local source in fixed bounded chunks before publishing any file.
    /// A source that grows, ends early, or disagrees with its expected hash fails.
    /// Read implementations must provide their own I/O deadlines; cancellation
    /// is checked between bounded reads, not during a blocking OS read.
    pub fn retain_stream(
        &mut self,
        expected: FileContent,
        mut source: impl Read,
        control: &JobControl,
    ) -> Result<VerifiedContentReceipt, ContentStoreError> {
        self.require_usable()?;
        if expected.byte_length() > MAX_SYNC_CONTENT_FILE_BYTES {
            return Err(ContentStoreError::InvalidContent);
        }
        // Detect unexpected externally introduced files before charging this run.
        self.rescan()?;
        check_control(control)?;
        let mut remaining = expected.byte_length();
        let mut whole = blake3::Hasher::new();
        let mut chunks = Vec::new();
        let count = expected
            .byte_length()
            .div_ceil(MAX_SYNC_CONTENT_CHUNK_BYTES as u64);
        chunks
            .try_reserve_exact(
                usize::try_from(count).map_err(|_| ContentStoreError::ResourceLimit)?,
            )
            .map_err(|_| ContentStoreError::ResourceLimit)?;
        while remaining > 0 {
            check_control(control)?;
            if chunks.len() >= MAX_SYNC_CONTENT_CHUNKS {
                return Err(ContentStoreError::ResourceLimit);
            }
            let length = remaining.min(MAX_SYNC_CONTENT_CHUNK_BYTES as u64) as usize;
            let mut chunk = Zeroizing::new(Vec::new());
            chunk
                .try_reserve_exact(length)
                .map_err(|_| ContentStoreError::ResourceLimit)?;
            chunk.resize(length, 0);
            let mut offset = 0;
            while offset < length {
                check_control(control)?;
                let end = length.min(offset + READ_STEP_BYTES);
                match source.read(&mut chunk[offset..end]) {
                    Ok(0) => return Err(ContentStoreError::InvalidContent),
                    Ok(read) if read <= end - offset => offset += read,
                    Ok(_) => return Err(ContentStoreError::SourceReadFailed),
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => return Err(ContentStoreError::SourceReadFailed),
                }
            }
            whole.update(&chunk);
            let descriptor = ChunkDescriptor::new(
                ChunkDigest::from_bytes(*blake3::hash(&chunk).as_bytes()),
                length as u32,
            )
            .map_err(|_| ContentStoreError::InvalidContent)?;
            self.retain_chunk(descriptor, &chunk, control)?;
            chunks.push(descriptor);
            remaining -= length as u64;
        }
        let mut extra = Zeroizing::new([0; 1]);
        loop {
            check_control(control)?;
            match source.read(extra.as_mut()) {
                Ok(0) => break,
                Ok(_) => return Err(ContentStoreError::InvalidContent),
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(ContentStoreError::SourceReadFailed),
            }
        }
        if whole.finalize().as_bytes() != &expected.digest().to_bytes() {
            return Err(ContentStoreError::InvalidContent);
        }
        let manifest = ContentManifest::new(expected.digest(), expected.byte_length(), chunks)
            .map_err(|_| ContentStoreError::InvalidContent)?;
        self.retain_manifest(expected, &manifest, control)
    }

    /// Reissues a receipt only after full content and durability verification.
    pub fn verify_retained(
        &self,
        expected: FileContent,
        control: &JobControl,
    ) -> Result<VerifiedContentReceipt, ContentStoreError> {
        self.require_usable()?;
        check_control(control)?;
        let file = self
            .open_object(Object::Manifest(expected))?
            .ok_or(ContentStoreError::MissingContent)?;
        let manifest = self
            .crypto
            .open_manifest(expected, &file.read_all().map_err(storage_error)?)
            .map_err(|_| ContentStoreError::InvalidContent)?;
        self.verify_chunks(expected, &manifest, control)?;
        file.sync_all(&self.manifests.lock).map_err(storage_error)?;
        self.manifests
            .dir
            .sync(&self.manifests.lock)
            .map_err(storage_error)?;
        check_control(control)?;
        Ok(VerifiedContentReceipt {
            domain: self.crypto.domain(),
            content: expected,
        })
    }

    /// Reads one reauthenticated bounded chunk for a staged apply or peer send.
    /// The caller must use an admitted manifest and authorize any peer access.
    pub fn read_chunk(
        &self,
        descriptor: ChunkDescriptor,
        control: &JobControl,
    ) -> Result<Zeroizing<Vec<u8>>, ContentStoreError> {
        self.require_usable()?;
        check_control(control)?;
        let file = self
            .open_object(Object::Chunk(descriptor))?
            .ok_or(ContentStoreError::MissingContent)?;
        let bytes = self
            .crypto
            .open_chunk(descriptor, &file.read_all().map_err(storage_error)?)
            .map_err(|_| ContentStoreError::InvalidContent)?;
        check_control(control)?;
        Ok(bytes)
    }

    /// Reads authenticated metadata; it does not claim that its chunks exist.
    pub fn read_manifest(
        &self,
        expected: FileContent,
    ) -> Result<ContentManifest, ContentStoreError> {
        self.require_usable()?;
        let file = self
            .open_object(Object::Manifest(expected))?
            .ok_or(ContentStoreError::MissingContent)?;
        self.crypto
            .open_manifest(expected, &file.read_all().map_err(storage_error)?)
            .map_err(|_| ContentStoreError::InvalidContent)
    }

    fn verify_chunks(
        &self,
        expected: FileContent,
        manifest: &ContentManifest,
        control: &JobControl,
    ) -> Result<(), ContentStoreError> {
        let mut whole = blake3::Hasher::new();
        let mut total = 0_u64;
        for descriptor in manifest.chunks() {
            check_control(control)?;
            let file = self
                .open_object(Object::Chunk(*descriptor))?
                .ok_or(ContentStoreError::MissingContent)?;
            let plaintext = self
                .crypto
                .open_chunk(*descriptor, &file.read_all().map_err(storage_error)?)
                .map_err(|_| ContentStoreError::InvalidContent)?;
            total = total
                .checked_add(plaintext.len() as u64)
                .ok_or(ContentStoreError::InvalidContent)?;
            if total > expected.byte_length() {
                return Err(ContentStoreError::InvalidContent);
            }
            whole.update(&plaintext);
            file.sync_all(&self.chunks.lock).map_err(storage_error)?;
        }
        if total != expected.byte_length()
            || whole.finalize().as_bytes() != &expected.digest().to_bytes()
        {
            return Err(ContentStoreError::InvalidContent);
        }
        self.chunks
            .dir
            .sync(&self.chunks.lock)
            .map_err(storage_error)?;
        check_control(control)
    }

    fn persist(
        &mut self,
        object: Object,
        record: &EncodedContentRecord,
    ) -> Result<(), ContentStoreError> {
        if let Some(file) = self.open_object(object)? {
            self.verify_object(object, &file)?;
            let destination = self.directory(object);
            file.sync_all(&destination.lock).map_err(storage_error)?;
            destination
                .dir
                .sync(&destination.lock)
                .map_err(storage_error)?;
            return Ok(());
        }
        let usage = ContentStoreUsage {
            stored_bytes: self
                .usage
                .stored_bytes
                .checked_add(record.as_bytes().len() as u64)
                .ok_or(ContentStoreError::ResourceLimit)?,
            objects: self
                .usage
                .objects
                .checked_add(1)
                .ok_or(ContentStoreError::ResourceLimit)?,
        };
        check_usage(usage, self.limits)?;
        let mut nonce = [0_u8; 32];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| ContentStoreError::EntropyUnavailable)?;
        let staging_key = internal_key(blake3::Hash::from(nonce).to_hex().as_str())?;
        let destination_key = self.locator(object).to_state_key().map_err(storage_error)?;
        // Any uncertain creation/promotion requires reopen and a complete rescan.
        self.poisoned = true;
        self.usage = usage;
        let source = self
            .staging
            .dir
            .create_new_file(
                &self.staging.lock,
                &staging_key,
                record.as_bytes(),
                object.limit() as u64,
            )
            .map_err(storage_error)?;
        let destination = self.directory(object);
        let promoted = destination
            .dir
            .promote_new_file(
                &destination.lock,
                &self.staging.lock,
                source,
                &destination_key,
            )
            .map_err(storage_error)?;
        match promoted {
            PrivateStatePromotion::Promoted(file) => self.verify_object(object, &file)?,
            PrivateStatePromotion::Existing {
                source: _,
                destination: file,
            } => {
                // The extra staged object stays permanently counted, never deleted.
                self.verify_object(object, &file)?;
                file.sync_all(&destination.lock).map_err(storage_error)?;
                destination
                    .dir
                    .sync(&destination.lock)
                    .map_err(storage_error)?;
            }
        }
        self.poisoned = false;
        Ok(())
    }

    fn verify_object(
        &self,
        object: Object,
        file: &PrivateStateFile,
    ) -> Result<(), ContentStoreError> {
        let record = file.read_all().map_err(storage_error)?;
        match object {
            Object::Chunk(expected) => {
                self.crypto
                    .open_chunk(expected, &record)
                    .map_err(|_| ContentStoreError::InvalidContent)?;
            }
            Object::Manifest(expected) => {
                self.crypto
                    .open_manifest(expected, &record)
                    .map_err(|_| ContentStoreError::InvalidContent)?;
            }
        }
        Ok(())
    }

    fn open_object(&self, object: Object) -> Result<Option<PrivateStateFile>, ContentStoreError> {
        let key = self.locator(object).to_state_key().map_err(storage_error)?;
        match self
            .directory(object)
            .dir
            .open_file(&key, object.limit() as u64)
        {
            Ok(file) => Ok(Some(file)),
            Err(StateDirError::Io {
                kind: ErrorKind::NotFound,
                ..
            }) => Ok(None),
            Err(error) => Err(storage_error(error)),
        }
    }

    fn directory(&self, object: Object) -> &LockedDirectory {
        match object {
            Object::Chunk(_) => &self.chunks,
            Object::Manifest(_) => &self.manifests,
        }
    }
    fn locator(&self, object: Object) -> ContentLocator {
        match object {
            Object::Chunk(expected) => self.crypto.chunk_locator(expected),
            Object::Manifest(expected) => self.crypto.manifest_locator(expected),
        }
    }

    fn rescan(&mut self) -> Result<(), ContentStoreError> {
        validate_root(&self.root)?;
        let mut usage = ContentStoreUsage::default();
        let entry_limit = self.limits.maximum_objects as usize + 1;
        for (directory, object_limit) in [
            (&self.chunks, MAX_ENCRYPTED_SYNC_CHUNK_BYTES),
            (&self.manifests, MAX_ENCRYPTED_SYNC_MANIFEST_BYTES),
            (&self.staging, MAX_ENCRYPTED_SYNC_MANIFEST_BYTES),
        ] {
            let inventory = directory
                .dir
                .inventory(&directory.lock, entry_limit, (entry_limit as u64) * 64)
                .map_err(storage_error)?;
            for entry in inventory.entries() {
                if entry.kind() == PrivateStateEntryKind::WriterLock {
                    continue;
                }
                if entry.kind() != PrivateStateEntryKind::RegularFile {
                    return Err(ContentStoreError::UnexpectedState);
                }
                let name = entry
                    .name()
                    .state_key()
                    .ok_or(ContentStoreError::UnexpectedState)?
                    .as_str();
                if name.len() != 64
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    || entry.byte_length() > object_limit as u64
                {
                    return Err(ContentStoreError::UnexpectedState);
                }
                usage.objects = usage
                    .objects
                    .checked_add(1)
                    .ok_or(ContentStoreError::ResourceLimit)?;
                usage.stored_bytes = usage
                    .stored_bytes
                    .checked_add(entry.byte_length())
                    .ok_or(ContentStoreError::ResourceLimit)?;
                check_usage(usage, self.limits)?;
            }
        }
        self.usage = usage;
        Ok(())
    }

    fn require_usable(&self) -> Result<(), ContentStoreError> {
        if self.poisoned {
            return Err(ContentStoreError::ReopenRequired);
        }
        self.root.lock.validate().map_err(storage_error)?;
        self.chunks.lock.validate().map_err(storage_error)?;
        self.manifests.lock.validate().map_err(storage_error)?;
        self.staging.lock.validate().map_err(storage_error)
    }
}

#[derive(Clone, Copy)]
enum Object {
    Chunk(ChunkDescriptor),
    Manifest(FileContent),
}
impl Object {
    const fn limit(self) -> usize {
        match self {
            Self::Chunk(_) => MAX_ENCRYPTED_SYNC_CHUNK_BYTES,
            Self::Manifest(_) => MAX_ENCRYPTED_SYNC_MANIFEST_BYTES,
        }
    }
}

fn validate_root(root: &LockedDirectory) -> Result<(), ContentStoreError> {
    let inventory = root
        .dir
        .inventory(&root.lock, 4, 4 * 64)
        .map_err(storage_error)?;
    for entry in inventory.entries() {
        if entry.kind() == PrivateStateEntryKind::WriterLock {
            continue;
        }
        if entry.kind() != PrivateStateEntryKind::Directory
            || !matches!(
                entry
                    .name()
                    .state_key()
                    .ok_or(ContentStoreError::UnexpectedState)?
                    .as_str(),
                "chunks" | "manifests" | "staging"
            )
        {
            return Err(ContentStoreError::UnexpectedState);
        }
    }
    Ok(())
}
fn internal_key(name: &str) -> Result<StateKey, ContentStoreError> {
    StateKey::new(name).map_err(storage_error)
}
fn check_usage(
    usage: ContentStoreUsage,
    limits: ContentStoreLimits,
) -> Result<(), ContentStoreError> {
    if usage.objects > limits.maximum_objects || usage.stored_bytes > limits.maximum_stored_bytes {
        return Err(ContentStoreError::ResourceLimit);
    }
    Ok(())
}
fn check_control(control: &JobControl) -> Result<(), ContentStoreError> {
    control.check().map_err(|_| ContentStoreError::Interrupted)
}
fn storage_error(error: StateDirError) -> ContentStoreError {
    match error {
        StateDirError::InventoryLimit | StateDirError::FileTooLarge => {
            ContentStoreError::ResourceLimit
        }
        StateDirError::Locked => ContentStoreError::Locked,
        _ => ContentStoreError::StorageFailed,
    }
}

/// Fixed failures that preserve private content and path confidentiality.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ContentStoreError {
    /// Caller limits are outside the finite supported range.
    #[error("invalid sync content store limits")]
    InvalidLimits,
    /// Finite object or byte capacity is exhausted.
    #[error("sync content storage limit reached")]
    ResourceLimit,
    /// A required final object is absent.
    #[error("sync content is not retained")]
    MissingContent,
    /// Bytes or ordered metadata do not match their content commitment.
    #[error("sync content verification failed")]
    InvalidContent,
    /// An unexpected private entry prevents complete safe accounting.
    #[error("unexpected sync content store entry")]
    UnexpectedState,
    /// Another cooperative process owns this cache.
    #[error("sync content store is locked")]
    Locked,
    /// Storage could not establish the required identity or durability.
    #[error("sync content storage operation failed")]
    StorageFailed,
    /// A previous uncertain mutation requires a fresh complete inventory.
    #[error("sync content store must be reopened")]
    ReopenRequired,
    /// OS entropy could not allocate a fresh staging name.
    #[error("sync content storage entropy is unavailable")]
    EntropyUnavailable,
    /// The source reader failed without exposing its potentially private error.
    #[error("sync content source read failed")]
    SourceReadFailed,
    /// Ingestion was paused or cancelled between bounded operations.
    #[error("sync content operation interrupted")]
    Interrupted,
}

#[cfg(test)]
#[path = "content_store_tests.rs"]
mod tests;
