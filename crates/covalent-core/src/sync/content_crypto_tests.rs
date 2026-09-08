use super::*;

use crate::sync::body::ContentDigest;
use crate::sync::content_manifest::{
    ChunkDigest, MAX_SYNC_CONTENT_CHUNKS, MAX_SYNC_CONTENT_FILE_BYTES,
};

fn folder(value: u128) -> FolderId {
    FolderId::from_uuid(Uuid::from_u128(value))
}

fn domain(folder_value: u128, installation: u128, generation: u128) -> SyncContentDomain {
    SyncContentDomain::new(
        folder(folder_value),
        Uuid::from_u128(installation),
        Uuid::from_u128(generation),
    )
}

fn crypto_with(domain: SyncContentDomain, secret: u8) -> SyncContentCrypto {
    SyncContentCrypto::from_bytes(domain, [secret; 32]).expect("crypto")
}

fn crypto() -> SyncContentCrypto {
    crypto_with(domain(1, 2, 3), 4)
}

fn chunk(bytes: &[u8]) -> ChunkDescriptor {
    ChunkDescriptor::new(
        ChunkDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
        bytes.len() as u32,
    )
    .expect("chunk")
}

fn file(bytes: &[u8], executable: bool) -> FileContent {
    FileContent::new(
        ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
        bytes.len() as u64,
        executable,
    )
    .expect("file")
}

fn manifest(bytes: &[u8], split: usize) -> (FileContent, ContentManifest) {
    let expected = file(bytes, false);
    let chunks = if bytes.is_empty() {
        Vec::new()
    } else {
        vec![chunk(&bytes[..split]), chunk(&bytes[split..])]
    };
    let manifest =
        ContentManifest::new(expected.digest(), expected.byte_length(), chunks).expect("manifest");
    (expected, manifest)
}

struct FixedNonce([u8; NONCE_BYTES]);

impl NonceSource for FixedNonce {
    fn fill(&mut self, nonce: &mut [u8; NONCE_BYTES]) -> Result<(), ContentCryptoError> {
        nonce.copy_from_slice(&self.0);
        Ok(())
    }
}

struct FailingNonce;

impl NonceSource for FailingNonce {
    fn fill(&mut self, _nonce: &mut [u8; NONCE_BYTES]) -> Result<(), ContentCryptoError> {
        Err(ContentCryptoError::EntropyUnavailable)
    }
}

#[test]
fn real_chunks_and_manifest_round_trip_with_executable_reuse() {
    let crypto = crypto();
    let first_bytes = b"first chunk";
    let second_bytes = b" and second chunk";
    for plaintext in [first_bytes.as_slice(), second_bytes.as_slice()] {
        let expected = chunk(plaintext);
        let record = crypto.seal_chunk(expected, plaintext).expect("seal chunk");
        assert_eq!(
            crypto
                .open_chunk(expected, record.as_bytes())
                .expect("open chunk")
                .as_slice(),
            plaintext
        );
    }

    let whole = [first_bytes.as_slice(), second_bytes.as_slice()].concat();
    let expected = file(&whole, false);
    let manifest = ContentManifest::new(
        expected.digest(),
        expected.byte_length(),
        vec![chunk(first_bytes), chunk(second_bytes)],
    )
    .expect("manifest");
    let record = crypto.seal_manifest(&manifest).expect("seal manifest");
    let executable = FileContent::new(expected.digest(), expected.byte_length(), true)
        .expect("executable reference");
    assert_eq!(
        crypto
            .open_manifest(executable, record.as_bytes())
            .expect("open manifest"),
        manifest
    );
    assert_eq!(
        crypto.manifest_locator(expected),
        crypto.manifest_locator(executable)
    );
}

#[test]
fn exact_empty_manifest_round_trips() {
    let crypto = crypto();
    let (expected, manifest) = manifest(b"", 0);
    let record = crypto
        .seal_manifest(&manifest)
        .expect("seal empty manifest");
    assert_eq!(
        crypto
            .open_manifest(expected, record.as_bytes())
            .expect("open empty manifest"),
        manifest
    );
    assert_eq!(
        record.as_bytes().len(),
        HEADER_BYTES + NONCE_BYTES + 55 + TAG_BYTES
    );
}

#[test]
fn wrong_key_and_each_domain_component_fail_authentication() {
    let expected = chunk(b"domain-bound chunk");
    let owner = crypto();
    let record = owner
        .seal_chunk(expected, b"domain-bound chunk")
        .expect("seal");
    let alternatives = [
        crypto_with(domain(1, 2, 3), 5),
        crypto_with(domain(9, 2, 3), 4),
        crypto_with(domain(1, 9, 3), 4),
        crypto_with(domain(1, 2, 9), 4),
    ];
    for alternative in alternatives {
        assert!(matches!(
            alternative.open_chunk(expected, record.as_bytes()),
            Err(ContentCryptoError::AuthenticationFailed)
        ));
    }
}

#[test]
fn wrong_kind_digest_and_length_cannot_substitute() {
    let crypto = crypto();
    let bytes = b"bound object";
    let expected = chunk(bytes);
    let record = crypto.seal_chunk(expected, bytes).expect("seal");

    assert!(matches!(
        crypto.open(
            ObjectBinding::manifest(file(bytes, false)),
            record.as_bytes()
        ),
        Err(ContentCryptoError::UnexpectedKind)
    ));
    let wrong_digest =
        ChunkDescriptor::new(ChunkDigest::from_bytes([91; 32]), expected.byte_length())
            .expect("wrong digest");
    assert!(matches!(
        crypto.open_chunk(wrong_digest, record.as_bytes()),
        Err(ContentCryptoError::AuthenticationFailed)
    ));
    let wrong_length =
        ChunkDescriptor::new(expected.digest(), expected.byte_length() - 1).expect("wrong length");
    assert!(matches!(
        crypto.open_chunk(wrong_length, record.as_bytes()),
        Err(ContentCryptoError::InvalidLength)
    ));

    let (_, manifest) = manifest(b"manifest body", 5);
    let manifest_record = crypto.seal_manifest(&manifest).expect("seal manifest");
    let wrong_file_digest = FileContent::new(
        ContentDigest::from_bytes([92; 32]),
        manifest.byte_length(),
        false,
    )
    .expect("wrong file digest");
    assert!(matches!(
        crypto.open_manifest(wrong_file_digest, manifest_record.as_bytes()),
        Err(ContentCryptoError::AuthenticationFailed)
    ));
    let wrong_file_length =
        FileContent::new(manifest.whole_digest(), manifest.byte_length() + 1, false)
            .expect("wrong file length");
    assert!(matches!(
        crypto.open_manifest(wrong_file_length, manifest_record.as_bytes()),
        Err(ContentCryptoError::AuthenticationFailed)
    ));
}

#[test]
fn every_truncated_prefix_and_trailing_byte_is_rejected() {
    let crypto = crypto();
    let bytes = b"prefix boundary";
    let expected = chunk(bytes);
    let record = crypto.seal_chunk(expected, bytes).expect("seal");
    for end in 0..record.as_bytes().len() {
        assert!(
            crypto
                .open_chunk(expected, &record.as_bytes()[..end])
                .is_err(),
            "accepted prefix {end}"
        );
    }
    let mut trailing = record.as_bytes().to_vec();
    trailing.push(0);
    assert!(matches!(
        crypto.open_chunk(expected, &trailing),
        Err(ContentCryptoError::InvalidLength)
    ));
}

#[test]
fn version_flags_kind_and_hostile_lengths_are_rejected_before_decryption() {
    let crypto = crypto();
    let bytes = b"header-bound";
    let expected = chunk(bytes);
    let record = crypto.seal_chunk(expected, bytes).expect("seal");

    let mut bad_magic = record.as_bytes().to_vec();
    bad_magic[0] ^= 1;
    assert!(matches!(
        crypto.open_chunk(expected, &bad_magic),
        Err(ContentCryptoError::InvalidVersion)
    ));
    let mut bad_version = record.as_bytes().to_vec();
    bad_version[9] = 2;
    assert!(matches!(
        crypto.open_chunk(expected, &bad_version),
        Err(ContentCryptoError::InvalidVersion)
    ));
    let mut bad_flags = record.as_bytes().to_vec();
    bad_flags[11] = 1;
    assert!(matches!(
        crypto.open_chunk(expected, &bad_flags),
        Err(ContentCryptoError::InvalidFlags)
    ));
    let mut bad_kind = record.as_bytes().to_vec();
    bad_kind[10] = ObjectKind::Manifest as u8;
    assert!(matches!(
        crypto.open_chunk(expected, &bad_kind),
        Err(ContentCryptoError::UnexpectedKind)
    ));
    let mut hostile_length = record.as_bytes().to_vec();
    hostile_length[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        crypto.open_chunk(expected, &hostile_length),
        Err(ContentCryptoError::InvalidLength)
    ));
    let oversized = vec![0; MAX_ENCRYPTED_SYNC_CHUNK_BYTES + 1];
    assert!(matches!(
        crypto.open_chunk(expected, &oversized),
        Err(ContentCryptoError::InvalidLength)
    ));
}

#[test]
fn every_authenticated_record_region_detects_mutation() {
    let crypto = crypto();
    let bytes = b"mutation target";
    let expected = chunk(bytes);
    let record = crypto.seal_chunk(expected, bytes).expect("seal");

    for offset in [HEADER_BYTES, PAYLOAD_OFFSET, record.as_bytes().len() - 1] {
        let mut changed = record.as_bytes().to_vec();
        changed[offset] ^= 1;
        assert!(matches!(
            crypto.open_chunk(expected, &changed),
            Err(ContentCryptoError::AuthenticationFailed)
        ));
    }
    let mut authenticated_header = record.as_bytes().to_vec();
    authenticated_header[12..16].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    authenticated_header[0] ^= 1;
    assert!(matches!(
        crypto.open_chunk(expected, &authenticated_header),
        Err(ContentCryptoError::InvalidVersion)
    ));
}

#[test]
fn public_seal_and_open_independently_enforce_plaintext_commitments() {
    let crypto = crypto();
    let expected = chunk(b"right");
    assert!(matches!(
        crypto.seal_chunk(expected, b"wr0ng"),
        Err(ContentCryptoError::ContentMismatch)
    ));
    assert!(matches!(
        crypto.seal_chunk(expected, b"tiny"),
        Err(ContentCryptoError::InvalidLength)
    ));

    let wrong_chunk_record = crypto
        .seal(
            ObjectBinding::chunk(expected),
            b"wr0ng",
            &mut FixedNonce([31; NONCE_BYTES]),
        )
        .expect("private authenticated chunk");
    assert_eq!(
        crypto
            .open(
                ObjectBinding::chunk(expected),
                wrong_chunk_record.as_bytes()
            )
            .expect("private authenticated open")
            .as_slice(),
        b"wr0ng"
    );
    assert!(matches!(
        crypto.open_chunk(expected, wrong_chunk_record.as_bytes()),
        Err(ContentCryptoError::ContentMismatch)
    ));

    let expected_file = file(b"right", true);
    let wrong_file = file(b"wr0ng", false);
    let wrong_manifest = ContentManifest::new(
        wrong_file.digest(),
        wrong_file.byte_length(),
        vec![chunk(b"wr0ng")],
    )
    .expect("wrong manifest");
    let wrong_manifest_bytes = wrong_manifest.encode().expect("encode wrong manifest");
    let wrong_manifest_record = crypto
        .seal(
            ObjectBinding::manifest(expected_file),
            &wrong_manifest_bytes,
            &mut FixedNonce([32; NONCE_BYTES]),
        )
        .expect("private authenticated manifest");
    assert_eq!(
        crypto
            .open(
                ObjectBinding::manifest(expected_file),
                wrong_manifest_record.as_bytes()
            )
            .expect("private authenticated open")
            .as_slice(),
        wrong_manifest_bytes.as_slice()
    );
    assert!(matches!(
        crypto.open_manifest(expected_file, wrong_manifest_record.as_bytes()),
        Err(ContentCryptoError::InvalidManifest)
    ));
}

#[test]
fn nonce_source_is_fallible_and_public_seals_use_distinct_nonces() {
    let crypto = crypto();
    let bytes = b"nonce diversity";
    let expected = chunk(bytes);
    assert!(matches!(
        crypto.seal(ObjectBinding::chunk(expected), bytes, &mut FailingNonce),
        Err(ContentCryptoError::EntropyUnavailable)
    ));

    let first = crypto.seal_chunk(expected, bytes).expect("first seal");
    let second = crypto.seal_chunk(expected, bytes).expect("second seal");
    assert_ne!(
        &first.as_bytes()[HEADER_BYTES..PAYLOAD_OFFSET],
        &second.as_bytes()[HEADER_BYTES..PAYLOAD_OFFSET]
    );
    assert_ne!(first.as_bytes(), second.as_bytes());
}

#[test]
fn locators_are_deterministic_and_separate_every_binding_dimension() {
    let owner = crypto();
    assert_eq!(owner.domain(), domain(1, 2, 3));
    assert_eq!(owner.domain().folder_id(), folder(1));
    assert_eq!(owner.domain().installation_id(), Uuid::from_u128(2));
    assert_eq!(owner.domain().generation_id(), Uuid::from_u128(3));

    let bytes = b"locator input";
    let expected = chunk(bytes);
    let locator = owner.chunk_locator(expected);
    assert_eq!(locator, owner.chunk_locator(expected));
    let same_digest_file = FileContent::new(
        ContentDigest::from_bytes(expected.digest().to_bytes()),
        u64::from(expected.byte_length()),
        false,
    )
    .expect("same digest file");
    assert_ne!(locator, owner.manifest_locator(same_digest_file));
    assert_ne!(
        locator,
        owner.chunk_locator(
            ChunkDescriptor::new(ChunkDigest::from_bytes([41; 32]), expected.byte_length())
                .expect("different digest")
        )
    );
    assert_ne!(
        locator,
        owner.chunk_locator(
            ChunkDescriptor::new(expected.digest(), expected.byte_length() - 1)
                .expect("different length")
        )
    );
    for alternative in [
        crypto_with(domain(1, 2, 3), 5),
        crypto_with(domain(9, 2, 3), 4),
        crypto_with(domain(1, 9, 3), 4),
        crypto_with(domain(1, 2, 9), 4),
    ] {
        assert_ne!(locator, alternative.chunk_locator(expected));
    }

    #[cfg(unix)]
    assert!(locator.to_state_key().is_ok());
}

#[test]
fn maximum_chunk_round_trips_at_the_exact_record_bound() {
    let crypto = crypto();
    let plaintext = vec![0x5a; MAX_SYNC_CONTENT_CHUNK_BYTES];
    let expected = chunk(&plaintext);
    let record = crypto
        .seal_chunk(expected, &plaintext)
        .expect("seal maximum chunk");
    assert_eq!(record.as_bytes().len(), MAX_ENCRYPTED_SYNC_CHUNK_BYTES);
    assert_eq!(
        crypto
            .open_chunk(expected, record.as_bytes())
            .expect("open maximum chunk")
            .as_slice(),
        plaintext
    );
}

#[test]
fn maximum_manifest_metadata_round_trips_without_file_allocation() {
    let crypto = crypto();
    let maximum_chunk = ChunkDescriptor::new(
        ChunkDigest::from_bytes([61; 32]),
        MAX_SYNC_CONTENT_CHUNK_BYTES as u32,
    )
    .expect("maximum chunk");
    let expected = FileContent::new(
        ContentDigest::from_bytes([62; 32]),
        MAX_SYNC_CONTENT_FILE_BYTES,
        true,
    )
    .expect("maximum file reference");
    let manifest = ContentManifest::new(
        expected.digest(),
        expected.byte_length(),
        vec![maximum_chunk; MAX_SYNC_CONTENT_CHUNKS],
    )
    .expect("maximum manifest");
    let record = crypto
        .seal_manifest(&manifest)
        .expect("seal maximum manifest");
    assert!(record.as_bytes().len() <= MAX_ENCRYPTED_SYNC_MANIFEST_BYTES);
    let opened = crypto
        .open_manifest(expected, record.as_bytes())
        .expect("open maximum manifest");
    assert_eq!(opened.byte_length(), MAX_SYNC_CONTENT_FILE_BYTES);
    assert_eq!(opened.chunks().len(), MAX_SYNC_CONTENT_CHUNKS);
}

#[test]
fn debug_and_errors_redact_keys_digests_and_plaintext() {
    let canary = b"private-content-canary";
    let crypto = crypto_with(domain(11, 12, 13), b'K');
    let expected = chunk(canary);
    let locator = crypto.chunk_locator(expected);
    let record = crypto.seal_chunk(expected, canary).expect("seal canary");
    let rendered = format!(
        "{crypto:?} {locator:?} {record:?} {:?} {}",
        ContentCryptoError::ContentMismatch,
        ContentCryptoError::AuthenticationFailed
    );
    assert!(!rendered.contains("private-content-canary"));
    assert!(!rendered.contains(&"4b".repeat(32)));
    assert!(
        !rendered.contains(
            blake3::Hash::from(expected.digest().to_bytes())
                .to_hex()
                .as_str()
        )
    );
    assert!(
        !record
            .as_bytes()
            .windows(canary.len())
            .any(|window| window == canary)
    );
}
