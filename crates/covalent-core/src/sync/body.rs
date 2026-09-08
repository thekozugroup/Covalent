//! Canonical path values carried inside a signed folder operation.
//!
//! Parsing does not authorize an operation or prove that referenced bytes are
//! available. A file receiver must verify the exact length and whole-file
//! plaintext BLAKE3 digest before applying staged content. This version carries
//! file bytes and an executable bit, not portable timestamps, ACLs or symlinks.

use std::fmt;

use thiserror::Error;

use super::operation::MAX_OPERATION_BODY_BYTES;
use super::path::{MAX_SYNC_PATH_BYTES, SyncPath};

const MAGIC: &[u8; 8] = b"COVSB001";
const FILE: u8 = 1;
const DIRECTORY: u8 = 2;
const TOMBSTONE: u8 = 3;
const EXECUTABLE: u8 = 1;
const PREFIX_BYTES: usize = MAGIC.len() + 1 + 1 + 2;

/// Whole-file plaintext commitment, distinct from an operation-record digest.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    /// Constructs a claimed commitment; content verification remains required.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed digest bytes for content verification.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ContentDigest([redacted])")
    }
}

/// An immutable file version whose content is transferred separately.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct FileContent {
    digest: ContentDigest,
    byte_length: u64,
    executable: bool,
}

impl FileContent {
    /// Validates a file reference without reading any content.
    ///
    /// Empty content has exactly one digest. Nonempty content and configured
    /// file-size/disk quotas must be checked by the cache and transfer layer.
    pub fn new(
        digest: ContentDigest,
        byte_length: u64,
        executable: bool,
    ) -> Result<Self, BodyError> {
        if byte_length == 0 && digest.0 != *blake3::hash(b"").as_bytes() {
            return Err(BodyError::InvalidEmptyFile);
        }
        Ok(Self {
            digest,
            byte_length,
            executable,
        })
    }

    /// Returns the claimed whole-file commitment.
    #[must_use]
    pub const fn digest(self) -> ContentDigest {
        self.digest
    }

    /// Returns the exact required plaintext length.
    #[must_use]
    pub const fn byte_length(self) -> u64 {
        self.byte_length
    }

    /// Returns the requested executable bit, subject to target capability.
    #[must_use]
    pub const fn executable(self) -> bool {
        self.executable
    }
}

impl fmt::Debug for FileContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileContent")
            .field("byte_length", &self.byte_length)
            .field("executable", &self.executable)
            .finish_non_exhaustive()
    }
}

/// A path-register value. Tombstones are retained authenticated history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryValue {
    /// One immutable file reference.
    File(FileContent),
    /// A directory, including an empty directory.
    Directory,
    /// A deletion proposal; concurrent content must survive conflict planning.
    Tombstone,
}

/// One canonical relative path and its authenticated proposed value.
#[derive(Clone, Eq, PartialEq)]
pub struct OperationBody {
    path: SyncPath,
    value: EntryValue,
}

impl fmt::Debug for OperationBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationBody")
            .field("path_bytes", &self.path.as_str().len())
            .field("value", &self.value)
            .finish()
    }
}

impl OperationBody {
    /// Builds a body from an already bounded canonical path and value.
    #[must_use]
    pub const fn new(path: SyncPath, value: EntryValue) -> Self {
        Self { path, value }
    }

    /// Returns the canonical shared path, never an absolute filesystem path.
    #[must_use]
    pub const fn path(&self) -> &SyncPath {
        &self.path
    }

    /// Returns the proposed value, before causal conflict resolution.
    #[must_use]
    pub const fn value(&self) -> EntryValue {
        self.value
    }

    /// Encodes exactly one body; construction bounds its maximum length.
    ///
    /// Wire order: magic, kind, flags, big-endian u16 path length, UTF-8 NFC
    /// path, then (for files only) big-endian u64 byte length and digest[32].
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(PREFIX_BYTES + self.path.as_str().len() + 40);
        bytes.extend_from_slice(MAGIC);
        let (kind, flags) = match self.value {
            EntryValue::File(file) => (FILE, u8::from(file.executable)),
            EntryValue::Directory => (DIRECTORY, 0),
            EntryValue::Tombstone => (TOMBSTONE, 0),
        };
        bytes.push(kind);
        bytes.push(flags);
        bytes.extend_from_slice(&(self.path.as_str().len() as u16).to_be_bytes());
        bytes.extend_from_slice(self.path.as_str().as_bytes());
        if let EntryValue::File(file) = self.value {
            bytes.extend_from_slice(&file.byte_length.to_be_bytes());
            bytes.extend_from_slice(&file.digest.0);
        }
        bytes
    }

    /// Decodes bounded canonical bytes without rewriting signed path spelling.
    pub fn decode(bytes: &[u8]) -> Result<Self, BodyError> {
        if bytes.len() > MAX_OPERATION_BODY_BYTES {
            return Err(BodyError::TooLarge);
        }
        let prefix = bytes.get(..PREFIX_BYTES).ok_or(BodyError::Truncated)?;
        if prefix[..8] != *MAGIC {
            return Err(BodyError::InvalidVersion);
        }
        let kind = prefix[8];
        let flags = prefix[9];
        let suffix_length = match kind {
            FILE if flags & !EXECUTABLE == 0 => 40,
            DIRECTORY | TOMBSTONE if flags == 0 => 0,
            FILE | DIRECTORY | TOMBSTONE => return Err(BodyError::InvalidFlags),
            _ => return Err(BodyError::InvalidKind),
        };
        let path_length = usize::from(u16::from_be_bytes([prefix[10], prefix[11]]));
        if path_length > MAX_SYNC_PATH_BYTES {
            return Err(BodyError::InvalidPath);
        }
        // Path and suffix are bounded constants, so both additions fit usize
        // on every supported target before any path allocation occurs.
        let path_end = PREFIX_BYTES + path_length;
        let expected_length = path_end + suffix_length;
        if bytes.len() < expected_length {
            return Err(BodyError::Truncated);
        }
        if bytes.len() != expected_length {
            return Err(BodyError::TrailingBytes);
        }
        let spelling = std::str::from_utf8(&bytes[PREFIX_BYTES..path_end])
            .map_err(|_| BodyError::InvalidPath)?;
        let path = SyncPath::from_wire(spelling).map_err(|_| BodyError::InvalidPath)?;
        let value = match kind {
            FILE => {
                let length = u64::from_be_bytes(
                    bytes[path_end..path_end + 8]
                        .try_into()
                        .map_err(|_| BodyError::Truncated)?,
                );
                let digest = ContentDigest::from_bytes(
                    bytes[path_end + 8..]
                        .try_into()
                        .map_err(|_| BodyError::Truncated)?,
                );
                EntryValue::File(FileContent::new(digest, length, flags == EXECUTABLE)?)
            }
            DIRECTORY => EntryValue::Directory,
            TOMBSTONE => EntryValue::Tombstone,
            _ => return Err(BodyError::InvalidKind),
        };
        Ok(Self { path, value })
    }
}

/// Fixed redacted failures; rejected path spelling is never included.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BodyError {
    /// The outer operation body bound was exceeded before parsing.
    #[error("sync body exceeds its byte limit")]
    TooLarge,
    /// Not all declared bytes are present.
    #[error("sync body is truncated")]
    Truncated,
    /// Extra bytes prevent canonical representation.
    #[error("sync body has trailing bytes")]
    TrailingBytes,
    /// The body magic or version is unsupported.
    #[error("unsupported sync body version")]
    InvalidVersion,
    /// The entry kind is unsupported.
    #[error("unsupported sync entry kind")]
    InvalidKind,
    /// Flags are reserved or invalid for this entry kind.
    #[error("invalid sync entry flags")]
    InvalidFlags,
    /// The path is unsafe, oversized, invalid UTF-8 or not NFC.
    #[error("invalid canonical sync path")]
    InvalidPath,
    /// Zero-length content must have the unique empty-file digest.
    #[error("empty sync file has an invalid content digest")]
    InvalidEmptyFile,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(length: u64) -> EntryValue {
        EntryValue::File(
            FileContent::new(
                ContentDigest::from_bytes(*blake3::hash(b"abc").as_bytes()),
                length,
                true,
            )
            .expect("file"),
        )
    }

    fn body(path: &str, value: EntryValue) -> OperationBody {
        OperationBody::new(SyncPath::from_wire(path).expect("path"), value)
    }

    #[test]
    fn file_directory_tombstone_and_empty_file_are_distinct_canonical_values() {
        let empty = EntryValue::File(
            FileContent::new(
                ContentDigest::from_bytes(*blake3::hash(b"").as_bytes()),
                0,
                false,
            )
            .expect("empty file"),
        );
        let values = [file(3), EntryValue::Directory, EntryValue::Tombstone, empty];
        let mut encodings = std::collections::BTreeSet::new();
        for value in values {
            let original = body("photos/café.txt", value);
            let encoded = original.encode();
            assert!(encodings.insert(encoded.clone()));
            let decoded = OperationBody::decode(&encoded).expect("decode");
            assert_eq!(decoded, original);
            assert_eq!(decoded.encode(), encoded);
        }
        assert_eq!(
            FileContent::new(ContentDigest::from_bytes([7; 32]), 0, false),
            Err(BodyError::InvalidEmptyFile)
        );
    }

    #[test]
    fn maximum_path_and_file_length_remain_bounded_without_content_allocation() {
        let mut path = vec!["x".repeat(255); 16].join("/");
        // Build exactly 4096 bytes with components at most 255 bytes.
        path.pop();
        path.push_str("/y");
        assert_eq!(path.len(), MAX_SYNC_PATH_BYTES);
        let original = body(&path, file(u64::MAX));
        let encoded = original.encode();
        assert!(encoded.len() <= MAX_OPERATION_BODY_BYTES);
        assert_eq!(OperationBody::decode(&encoded).expect("decode"), original);
    }

    #[test]
    fn every_truncated_prefix_and_extra_bytes_are_rejected() {
        for value in [file(3), EntryValue::Directory, EntryValue::Tombstone] {
            let record = body("file.txt", value).encode();
            for end in 0..record.len() {
                assert!(OperationBody::decode(&record[..end]).is_err());
            }
            let mut extra = record;
            extra.push(0);
            assert_eq!(OperationBody::decode(&extra), Err(BodyError::TrailingBytes));
        }
        assert_eq!(
            OperationBody::decode(&vec![0; MAX_OPERATION_BODY_BYTES + 1]),
            Err(BodyError::TooLarge)
        );
    }

    #[test]
    fn hostile_path_bytes_are_never_normalized_or_returned_in_errors() {
        for spelling in [
            "../SECRET",
            "/SECRET",
            "SECRET\\file",
            "SECRET\0file",
            "cafe\u{301}",
            "",
        ] {
            let mut record = MAGIC.to_vec();
            record.extend_from_slice(&[DIRECTORY, 0]);
            record.extend_from_slice(&(spelling.len() as u16).to_be_bytes());
            record.extend_from_slice(spelling.as_bytes());
            let error = OperationBody::decode(&record).expect_err("invalid path");
            assert_eq!(error, BodyError::InvalidPath);
            assert!(!format!("{error:?} {error}").contains("SECRET"));
        }
        let mut invalid_utf8 = body("x", EntryValue::Directory).encode();
        invalid_utf8[PREFIX_BYTES] = 0xff;
        assert_eq!(
            OperationBody::decode(&invalid_utf8),
            Err(BodyError::InvalidPath)
        );
        let mut oversized_path = body("x", EntryValue::Directory).encode();
        oversized_path[10..12].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(
            OperationBody::decode(&oversized_path),
            Err(BodyError::InvalidPath)
        );
    }

    #[test]
    fn unknown_kinds_versions_and_kind_specific_flags_fail_closed() {
        let base = body("x", EntryValue::Directory).encode();
        let mut version = base.clone();
        version[7] = b'2';
        assert_eq!(
            OperationBody::decode(&version),
            Err(BodyError::InvalidVersion)
        );
        let mut kind = base.clone();
        kind[8] = 0;
        assert_eq!(OperationBody::decode(&kind), Err(BodyError::InvalidKind));
        let mut flags = base;
        flags[9] = EXECUTABLE;
        assert_eq!(OperationBody::decode(&flags), Err(BodyError::InvalidFlags));
        let mut file_flags = body("x", file(3)).encode();
        file_flags[9] = 2;
        assert_eq!(
            OperationBody::decode(&file_flags),
            Err(BodyError::InvalidFlags)
        );
    }

    #[test]
    fn debug_does_not_reveal_paths_or_content_commitments() {
        let original = body("SECRET-PATH", file(3));
        let debug = format!("{original:?}");
        assert!(!debug.contains("SECRET"));
        assert!(!debug.contains(&blake3::hash(b"abc").to_hex().to_string()));
    }
}
