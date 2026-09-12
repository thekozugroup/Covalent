use super::*;

use std::fs;
use std::io::{self, Cursor};
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use crate::sync::body::ContentDigest;

const STORE_BYTES: u64 = 128 * 1_024 * 1_024;
const STORE_OBJECTS: u64 = 1_024;

fn domain(folder: u128, installation: u128, generation: u128) -> SyncContentDomain {
    SyncContentDomain::new(
        super::super::ids::FolderId::from_uuid(uuid::Uuid::from_u128(folder)),
        uuid::Uuid::from_u128(installation),
        uuid::Uuid::from_u128(generation),
    )
}

fn default_domain() -> SyncContentDomain {
    domain(1, 2, 3)
}

fn crypto(domain: SyncContentDomain) -> SyncContentCrypto {
    SyncContentCrypto::from_bytes(domain, [4; 32]).expect("crypto")
}

fn limits() -> ContentStoreLimits {
    ContentStoreLimits {
        maximum_stored_bytes: STORE_BYTES,
        maximum_objects: STORE_OBJECTS,
    }
}

fn private_root(path: &Path) -> PrivateStateDir {
    PrivateStateDir::open_root(path).expect("private root")
}

fn new_root() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temp root");
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).expect("private mode");
    temp
}

fn open_store(
    path: &Path,
    domain: SyncContentDomain,
    limits: ContentStoreLimits,
) -> Result<SyncContentStore, ContentStoreError> {
    SyncContentStore::open(private_root(path), crypto(domain), limits)
}

fn fixture() -> (tempfile::TempDir, SyncContentStore) {
    let temp = new_root();
    let store = open_store(temp.path(), default_domain(), limits()).expect("store");
    (temp, store)
}

fn content(bytes: &[u8], executable: bool) -> FileContent {
    FileContent::new(
        ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
        bytes.len() as u64,
        executable,
    )
    .expect("content")
}

fn descriptor(bytes: &[u8]) -> ChunkDescriptor {
    ChunkDescriptor::new(
        ChunkDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
        bytes.len() as u32,
    )
    .expect("descriptor")
}

fn object_name(locator: ContentLocator) -> String {
    blake3::Hash::from(locator.to_bytes()).to_hex().to_string()
}

fn chunk_path(root: &Path, descriptor: ChunkDescriptor) -> PathBuf {
    root.join("chunks").join(object_name(
        crypto(default_domain()).chunk_locator(descriptor),
    ))
}

fn manifest_path(root: &Path, expected: FileContent) -> PathBuf {
    root.join("manifests").join(object_name(
        crypto(default_domain()).manifest_locator(expected),
    ))
}

fn non_lock_entries(path: &Path) -> Vec<String> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != "writer.lock")
        .collect();
    entries.sort();
    entries
}

fn read_all_chunks(
    store: &SyncContentStore,
    manifest: &ContentManifest,
    control: &JobControl,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    for chunk in manifest.chunks() {
        bytes.extend_from_slice(&store.read_chunk(*chunk, control).expect("read chunk"));
    }
    bytes
}

#[test]
fn multichunk_stream_round_trips_and_reuses_executable_content() {
    let (temp, mut store) = fixture();
    let mut bytes = vec![0x5a; MAX_SYNC_CONTENT_CHUNK_BYTES];
    bytes.extend((0_u8..=251).cycle().take(131_111));
    let expected = content(&bytes, false);
    let control = JobControl::new();
    let receipt = store
        .retain_stream(expected, Cursor::new(&bytes), &control)
        .expect("retain stream");
    assert_eq!(receipt.domain(), default_domain());
    assert_eq!(receipt.content(), expected);
    let usage = store.usage().expect("usage");
    assert_eq!(usage.objects, 3);

    let manifest = store.read_manifest(expected).expect("read manifest");
    assert_eq!(manifest.chunks().len(), 2);
    let readback = read_all_chunks(&store, &manifest, &control);
    assert_eq!(readback, bytes);
    assert_eq!(
        blake3::hash(&readback).as_bytes(),
        &expected.digest().to_bytes()
    );

    let executable =
        FileContent::new(expected.digest(), expected.byte_length(), true).expect("executable");
    let executable_receipt = store
        .retain_stream(executable, Cursor::new(&bytes), &control)
        .expect("reuse executable");
    assert_eq!(executable_receipt.content(), executable);
    assert_eq!(store.usage().expect("unchanged usage"), usage);
    assert_eq!(
        store
            .read_manifest(executable)
            .expect("executable manifest"),
        manifest
    );

    let manifest_ciphertext =
        fs::read(manifest_path(temp.path(), expected)).expect("manifest file");
    assert!(!manifest_ciphertext.is_empty());
}

#[test]
fn empty_stream_has_one_reusable_manifest_and_no_chunks() {
    let (_temp, mut store) = fixture();
    let expected = content(b"", false);
    let control = JobControl::new();
    store
        .retain_stream(expected, Cursor::new(Vec::<u8>::new()), &control)
        .expect("retain empty");
    let usage = store.usage().expect("usage");
    assert_eq!(usage.objects, 1);
    let manifest = store.read_manifest(expected).expect("empty manifest");
    assert!(manifest.chunks().is_empty());
    assert_eq!(manifest.byte_length(), 0);

    let executable = FileContent::new(expected.digest(), 0, true).expect("executable empty");
    store
        .retain_stream(executable, Cursor::new(Vec::<u8>::new()), &control)
        .expect("reuse empty");
    assert_eq!(store.usage().expect("unchanged usage"), usage);
}

#[test]
fn reopen_reverifies_retained_content_and_enforces_domain_binding() {
    let (temp, mut store) = fixture();
    let bytes = b"reopen content";
    let expected = content(bytes, true);
    let control = JobControl::new();
    store
        .retain_stream(expected, Cursor::new(bytes), &control)
        .expect("retain");
    let usage = store.usage().expect("usage");
    drop(store);

    let store = open_store(temp.path(), default_domain(), limits()).expect("reopen");
    let receipt = store
        .verify_retained(expected, &control)
        .expect("verify reopened");
    assert_eq!(receipt.domain(), default_domain());
    assert_eq!(store.usage().expect("reopened usage"), usage);
    drop(store);

    for wrong in [domain(9, 2, 3), domain(1, 9, 3), domain(1, 2, 9)] {
        let store = open_store(temp.path(), wrong, limits()).expect("wrong-domain store");
        assert!(matches!(
            store.verify_retained(expected, &control),
            Err(ContentStoreError::MissingContent)
        ));
        drop(store);
    }
}

#[test]
fn duplicate_chunk_and_manifest_retention_do_not_change_usage() {
    let (_temp, mut store) = fixture();
    let bytes = b"exact duplicate";
    let descriptor = descriptor(bytes);
    let expected = content(bytes, false);
    let manifest =
        ContentManifest::new(expected.digest(), expected.byte_length(), vec![descriptor])
            .expect("manifest");
    let control = JobControl::new();
    store
        .retain_chunk(descriptor, bytes, &control)
        .expect("first chunk");
    let chunk_usage = store.usage().expect("chunk usage");
    store
        .retain_chunk(descriptor, bytes, &control)
        .expect("duplicate chunk");
    assert_eq!(store.usage().expect("same chunk usage"), chunk_usage);
    store
        .retain_manifest(expected, &manifest, &control)
        .expect("first manifest");
    let complete_usage = store.usage().expect("complete usage");
    store
        .retain_manifest(expected, &manifest, &control)
        .expect("duplicate manifest");
    assert_eq!(store.usage().expect("same complete usage"), complete_usage);
}

#[test]
fn repeated_ordered_chunks_are_preserved_and_verified() {
    let (_temp, mut store) = fixture();
    let first = b"repeat";
    let middle = b"middle";
    let whole = [first.as_slice(), middle.as_slice(), first.as_slice()].concat();
    let expected = content(&whole, false);
    let repeated = descriptor(first);
    let middle_descriptor = descriptor(middle);
    let manifest = ContentManifest::new(
        expected.digest(),
        expected.byte_length(),
        vec![repeated, middle_descriptor, repeated],
    )
    .expect("manifest");
    let control = JobControl::new();
    store
        .retain_chunk(repeated, first, &control)
        .expect("repeat chunk");
    store
        .retain_chunk(middle_descriptor, middle, &control)
        .expect("middle chunk");
    store
        .retain_manifest(expected, &manifest, &control)
        .expect("retain manifest");
    let reopened = store.read_manifest(expected).expect("read manifest");
    assert_eq!(reopened.chunks(), &[repeated, middle_descriptor, repeated]);
    assert_eq!(read_all_chunks(&store, &reopened, &control), whole);
    assert_eq!(store.usage().expect("usage").objects, 3);
}

#[test]
fn wrong_whole_hash_missing_chunk_and_corrupt_chunk_issue_no_receipt() {
    let (temp, mut store) = fixture();
    let first = b"first";
    let second = b"second";
    let whole = [first.as_slice(), second.as_slice()].concat();
    let expected = content(&whole, false);
    let first_descriptor = descriptor(first);
    let second_descriptor = descriptor(second);
    let manifest = ContentManifest::new(
        expected.digest(),
        expected.byte_length(),
        vec![first_descriptor, second_descriptor],
    )
    .expect("manifest");
    let control = JobControl::new();
    store
        .retain_chunk(first_descriptor, first, &control)
        .expect("first chunk");
    assert!(matches!(
        store.retain_manifest(expected, &manifest, &control),
        Err(ContentStoreError::MissingContent)
    ));
    assert!(!manifest_path(temp.path(), expected).exists());

    store
        .retain_chunk(second_descriptor, second, &control)
        .expect("second chunk");
    let wrong_expected = FileContent::new(
        ContentDigest::from_bytes([31; 32]),
        expected.byte_length(),
        false,
    )
    .expect("wrong expected");
    assert!(matches!(
        store.retain_manifest(wrong_expected, &manifest, &control),
        Err(ContentStoreError::InvalidContent)
    ));
    assert!(!manifest_path(temp.path(), wrong_expected).exists());

    drop(store);
    let corrupt_path = chunk_path(temp.path(), second_descriptor);
    let corrupt = vec![0x99; fs::metadata(&corrupt_path).expect("metadata").len() as usize];
    fs::write(&corrupt_path, &corrupt).expect("corrupt chunk");
    let mut store = open_store(temp.path(), default_domain(), limits()).expect("reopen corrupt");
    assert!(matches!(
        store.retain_manifest(expected, &manifest, &control),
        Err(ContentStoreError::InvalidContent)
    ));
    assert!(!manifest_path(temp.path(), expected).exists());
    assert_eq!(
        fs::read(corrupt_path).expect("preserved corruption"),
        corrupt
    );
}

struct FailingReader {
    bytes: Vec<u8>,
    position: usize,
    fail_at: usize,
    message: &'static str,
}

impl Read for FailingReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.fail_at {
            return Err(io::Error::other(self.message));
        }
        let end = self
            .bytes
            .len()
            .min(self.fail_at)
            .min(self.position + output.len());
        let count = end.saturating_sub(self.position);
        output[..count].copy_from_slice(&self.bytes[self.position..end]);
        self.position = end;
        Ok(count)
    }
}

#[test]
fn eof_extra_bytes_and_source_errors_return_no_complete_receipt() {
    let (_temp, mut store) = fixture();
    let control = JobControl::new();
    let expected = content(b"expected bytes", false);
    assert!(matches!(
        store.retain_stream(expected, Cursor::new(b"expected byte"), &control),
        Err(ContentStoreError::InvalidContent)
    ));
    assert!(matches!(
        store.retain_stream(expected, Cursor::new(b"expected bytes!"), &control),
        Err(ContentStoreError::InvalidContent)
    ));
    assert!(matches!(
        store.verify_retained(expected, &control),
        Err(ContentStoreError::MissingContent)
    ));

    let reader = FailingReader {
        bytes: b"expected bytes".to_vec(),
        position: 0,
        fail_at: b"expected bytes".len(),
        message: "private-reader-canary",
    };
    let error = store
        .retain_stream(expected, reader, &control)
        .expect_err("source error");
    assert_eq!(error, ContentStoreError::SourceReadFailed);
    assert!(!format!("{error:?} {error}").contains("private-reader-canary"));
    assert!(matches!(
        store.verify_retained(expected, &control),
        Err(ContentStoreError::MissingContent)
    ));
}

struct CancelReader {
    bytes: Vec<u8>,
    position: usize,
    cancel_at: usize,
    control: JobControl,
}

impl Read for CancelReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.cancel_at {
            self.control.cancel();
        }
        let end = self.bytes.len().min(self.position + output.len());
        let count = end.saturating_sub(self.position);
        output[..count].copy_from_slice(&self.bytes[self.position..end]);
        self.position = end;
        Ok(count)
    }
}

#[test]
fn interrupted_stream_returns_no_receipt_and_retains_completed_chunks() {
    let (temp, mut store) = fixture();
    let mut bytes = vec![0x44; MAX_SYNC_CONTENT_CHUNK_BYTES];
    bytes.extend(vec![0x45; READ_STEP_BYTES * 2]);
    let expected = content(&bytes, false);
    let control = JobControl::new();
    let reader = CancelReader {
        bytes,
        position: 0,
        cancel_at: MAX_SYNC_CONTENT_CHUNK_BYTES,
        control: control.clone(),
    };
    assert!(matches!(
        store.retain_stream(expected, reader, &control),
        Err(ContentStoreError::Interrupted)
    ));
    let partial_usage = store.usage().expect("partial usage");
    assert_eq!(partial_usage.objects, 1);
    assert!(!manifest_path(temp.path(), expected).exists());
    drop(store);

    let store = open_store(temp.path(), default_domain(), limits()).expect("reopen partial");
    assert_eq!(store.usage().expect("recounted partial"), partial_usage);
    assert!(matches!(
        store.verify_retained(expected, &JobControl::new()),
        Err(ContentStoreError::MissingContent)
    ));
}

#[test]
fn source_failure_and_abandoned_staging_are_permanently_recounted() {
    let (temp, mut store) = fixture();
    let mut bytes = vec![0x71; MAX_SYNC_CONTENT_CHUNK_BYTES];
    bytes.extend(vec![0x72; READ_STEP_BYTES]);
    let expected = content(&bytes, false);
    let reader = FailingReader {
        bytes,
        position: 0,
        fail_at: MAX_SYNC_CONTENT_CHUNK_BYTES + 7,
        message: "private-source-failure",
    };
    assert!(matches!(
        store.retain_stream(expected, reader, &JobControl::new()),
        Err(ContentStoreError::SourceReadFailed)
    ));
    let failed_usage = store.usage().expect("failed usage");
    assert_eq!(failed_usage.objects, 1);
    drop(store);

    let staging_name = "a5".repeat(32);
    let staging_path = temp.path().join("staging").join(staging_name);
    fs::write(&staging_path, b"abandoned-staging").expect("stage");
    fs::set_permissions(&staging_path, fs::Permissions::from_mode(0o600)).expect("stage mode");
    let store = open_store(temp.path(), default_domain(), limits()).expect("reopen staged");
    assert_eq!(
        store.usage().expect("recounted staged"),
        ContentStoreUsage {
            stored_bytes: failed_usage.stored_bytes + b"abandoned-staging".len() as u64,
            objects: failed_usage.objects + 1,
        }
    );
}

#[test]
fn byte_and_object_limits_reject_before_creating_an_object() {
    let temp = new_root();
    for invalid in [
        ContentStoreLimits {
            maximum_stored_bytes: 0,
            maximum_objects: 1,
        },
        ContentStoreLimits {
            maximum_stored_bytes: 1,
            maximum_objects: 0,
        },
        ContentStoreLimits {
            maximum_stored_bytes: 1,
            maximum_objects: MAX_STORE_OBJECTS + 1,
        },
    ] {
        assert!(matches!(
            open_store(temp.path(), default_domain(), invalid),
            Err(ContentStoreError::InvalidLimits)
        ));
    }

    let byte_limits = ContentStoreLimits {
        maximum_stored_bytes: 1,
        maximum_objects: 4,
    };
    let mut store = open_store(temp.path(), default_domain(), byte_limits).expect("byte store");
    assert!(matches!(
        store.retain_chunk(descriptor(b"x"), b"x", &JobControl::new()),
        Err(ContentStoreError::ResourceLimit)
    ));
    assert_eq!(
        store.usage().expect("zero usage"),
        ContentStoreUsage::default()
    );
    assert!(non_lock_entries(&temp.path().join("chunks")).is_empty());
    assert!(non_lock_entries(&temp.path().join("staging")).is_empty());
    drop(store);

    let object_limits = ContentStoreLimits {
        maximum_stored_bytes: STORE_BYTES,
        maximum_objects: 1,
    };
    let mut store = open_store(temp.path(), default_domain(), object_limits).expect("object store");
    store
        .retain_chunk(descriptor(b"one"), b"one", &JobControl::new())
        .expect("first object");
    let usage = store.usage().expect("one object");
    assert!(matches!(
        store.retain_chunk(descriptor(b"two"), b"two", &JobControl::new()),
        Err(ContentStoreError::ResourceLimit)
    ));
    assert_eq!(store.usage().expect("unchanged usage"), usage);
    assert_eq!(non_lock_entries(&temp.path().join("chunks")).len(), 1);
    assert!(non_lock_entries(&temp.path().join("staging")).is_empty());
}

#[test]
fn pre_cancelled_control_writes_nothing() {
    let (temp, mut store) = fixture();
    let control = JobControl::new();
    control.cancel();
    let expected = content(b"cancelled-canary", false);
    assert!(matches!(
        store.retain_stream(expected, Cursor::new(b"cancelled-canary"), &control),
        Err(ContentStoreError::Interrupted)
    ));
    assert_eq!(
        store.usage().expect("zero usage"),
        ContentStoreUsage::default()
    );
    assert!(non_lock_entries(&temp.path().join("chunks")).is_empty());
    assert!(non_lock_entries(&temp.path().join("manifests")).is_empty());
    assert!(non_lock_entries(&temp.path().join("staging")).is_empty());
}

fn reopen_result(path: &Path) -> Result<SyncContentStore, ContentStoreError> {
    open_store(path, default_domain(), limits())
}

#[test]
fn unknown_root_and_child_entries_are_rejected() {
    let root_unknown = new_root();
    let unknown = root_unknown.path().join("unknown");
    fs::write(&unknown, b"unknown").expect("unknown root file");
    fs::set_permissions(&unknown, fs::Permissions::from_mode(0o600)).expect("unknown mode");
    assert!(matches!(
        reopen_result(root_unknown.path()),
        Err(ContentStoreError::UnexpectedState)
    ));

    let child_unknown = new_root();
    drop(reopen_result(child_unknown.path()).expect("initialize"));
    let unknown = child_unknown.path().join("chunks").join("unknown");
    fs::write(&unknown, b"unknown").expect("unknown child file");
    fs::set_permissions(&unknown, fs::Permissions::from_mode(0o600)).expect("unknown mode");
    assert!(matches!(
        reopen_result(child_unknown.path()),
        Err(ContentStoreError::UnexpectedState)
    ));
}

#[test]
fn symlinked_root_child_and_hardlinked_files_are_rejected() {
    let root_link = new_root();
    let target = root_link.path().join("target");
    fs::create_dir(&target).expect("target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).expect("target mode");
    let link = root_link.path().join("root-link");
    symlink(&target, &link).expect("root symlink");
    assert!(PrivateStateDir::open_root(&link).is_err());

    let child_link = new_root();
    drop(reopen_result(child_link.path()).expect("initialize"));
    let external = child_link.path().join("external");
    fs::write(&external, b"external").expect("external");
    let linked_name = "b6".repeat(32);
    symlink(
        &external,
        child_link.path().join("chunks").join(linked_name),
    )
    .expect("child link");
    assert!(matches!(
        reopen_result(child_link.path()),
        Err(ContentStoreError::StorageFailed)
    ));

    let hard_link = new_root();
    drop(reopen_result(hard_link.path()).expect("initialize"));
    let first = hard_link.path().join("chunks").join("c7".repeat(32));
    let second = hard_link.path().join("staging").join("d8".repeat(32));
    fs::write(&first, b"hard-linked").expect("first link");
    fs::set_permissions(&first, fs::Permissions::from_mode(0o600)).expect("link mode");
    fs::hard_link(&first, &second).expect("hard link");
    assert!(matches!(
        reopen_result(hard_link.path()),
        Err(ContentStoreError::StorageFailed)
    ));
}

#[test]
fn one_live_store_excludes_a_second_cooperative_owner() {
    let (temp, store) = fixture();
    assert!(matches!(
        open_store(temp.path(), default_domain(), limits()),
        Err(ContentStoreError::Locked)
    ));
    drop(store);
    assert!(open_store(temp.path(), default_domain(), limits()).is_ok());
}

#[test]
fn corrupt_incumbent_is_preserved_and_never_overwritten() {
    let (temp, mut store) = fixture();
    let bytes = b"incumbent plaintext";
    let expected = descriptor(bytes);
    store
        .retain_chunk(expected, bytes, &JobControl::new())
        .expect("retain incumbent");
    let path = chunk_path(temp.path(), expected);
    drop(store);

    let corruption = vec![0xee; fs::metadata(&path).expect("metadata").len() as usize];
    fs::write(&path, &corruption).expect("corrupt incumbent");
    let mut store = reopen_result(temp.path()).expect("reopen corrupt");
    let usage = store.usage().expect("usage");
    assert!(matches!(
        store.retain_chunk(expected, bytes, &JobControl::new()),
        Err(ContentStoreError::InvalidContent)
    ));
    assert_eq!(store.usage().expect("usage unchanged"), usage);
    assert_eq!(fs::read(path).expect("preserved bytes"), corruption);
}

#[test]
fn receipt_debug_and_store_errors_redact_private_canaries() {
    let (temp, mut store) = fixture();
    let canary = b"private-content-store-canary";
    let expected = content(canary, true);
    let receipt = store
        .retain_stream(expected, Cursor::new(canary), &JobControl::new())
        .expect("receipt");
    let rendered = format!(
        "{receipt:?} {store:?} {:?} {}",
        ContentStoreError::InvalidContent,
        ContentStoreError::SourceReadFailed
    );
    assert!(!rendered.contains("private-content-store-canary"));
    assert!(
        !rendered.contains(
            blake3::Hash::from(expected.digest().to_bytes())
                .to_hex()
                .as_str()
        )
    );
    for directory in ["chunks", "manifests", "staging"] {
        for name in non_lock_entries(&temp.path().join(directory)) {
            let bytes = fs::read(temp.path().join(directory).join(name)).expect("object");
            assert!(!bytes.windows(canary.len()).any(|window| window == canary));
        }
    }
}

#[test]
fn strict_reopen_retains_content_and_lifetime_exclusion() {
    let (temporary, mut store) = fixture();
    let bytes = b"strict reopen preserves complete retained content";
    let expected = content(bytes, false);
    store
        .retain_stream(expected, bytes.as_slice(), &JobControl::new())
        .unwrap();
    let usage = store.usage().unwrap();
    drop(store);
    let reopened = SyncContentStore::open_existing(
        private_root(temporary.path()),
        crypto(default_domain()),
        limits(),
    )
    .unwrap();
    assert_eq!(reopened.usage().unwrap(), usage);
    reopened
        .verify_retained(expected, &JobControl::new())
        .unwrap();
    assert!(
        SyncContentStore::open_existing(
            private_root(temporary.path()),
            crypto(default_domain()),
            limits()
        )
        .is_err()
    );
    drop(reopened);
    assert!(
        SyncContentStore::open_existing(
            private_root(temporary.path()),
            crypto(default_domain()),
            limits()
        )
        .is_ok()
    );
}

#[test]
fn strict_reopen_never_recreates_any_missing_directory_or_lock() {
    let empty = new_root();
    assert!(
        SyncContentStore::open_existing(
            private_root(empty.path()),
            crypto(default_domain()),
            limits()
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(empty.path()).unwrap().count(), 0);
    for missing in [
        "writer.lock",
        "chunks",
        "manifests",
        "staging",
        "chunks/writer.lock",
        "manifests/writer.lock",
        "staging/writer.lock",
    ] {
        let (temporary, store) = fixture();
        drop(store);
        let quarantine = new_root();
        let displaced = quarantine.path().join("preserved");
        fs::rename(temporary.path().join(missing), &displaced).unwrap();
        assert!(
            SyncContentStore::open_existing(
                private_root(temporary.path()),
                crypto(default_domain()),
                limits()
            )
            .is_err(),
            "missing {missing}"
        );
        assert!(
            !temporary.path().join(missing).exists(),
            "recreated {missing}"
        );
        assert!(displaced.exists());
        fs::rename(&displaced, temporary.path().join(missing)).unwrap();
        assert!(
            SyncContentStore::open_existing(
                private_root(temporary.path()),
                crypto(default_domain()),
                limits()
            )
            .is_ok()
        );
    }
}
