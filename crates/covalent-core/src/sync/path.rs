//! Bounded canonical paths for a future folder-sync protocol.
//!
//! These checks do not prove that a path can be created on a particular
//! filesystem. Apply code must still probe the actual target without
//! clobbering an existing entry and report unsupported source paths without
//! silently renaming them.

use std::fmt;
use std::str::FromStr;

use caseless::Caseless;
use covalent_protocol::{ContractError, RelativePath};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use unicode_normalization::{UnicodeNormalization, is_nfc};

/// Maximum UTF-8 bytes in one canonical synchronized path.
pub const MAX_SYNC_PATH_BYTES: usize = 4_096;
/// Maximum UTF-8 bytes in one synchronized path component.
pub const MAX_SYNC_PATH_COMPONENT_BYTES: usize = 255;
/// Maximum components in one synchronized path.
pub const MAX_SYNC_PATH_DEPTH: usize = 128;

/// A rejected synchronized-path invariant.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SyncPathError {
    /// Input or normalized output exceeded the protocol-wide byte bound.
    #[error("sync path exceeds 4096 UTF-8 bytes")]
    TooLong,
    /// Signed wire paths must already use their one canonical Unicode form.
    #[error("wire sync path is not Unicode NFC")]
    NonCanonicalUnicode,
    /// A folder tree deeper than this bound would make traversal unbounded.
    #[error("sync path exceeds 128 components")]
    TooDeep,
    /// Control characters have no safe portable user-facing representation.
    #[error("sync path contains a control character")]
    ControlCharacter,
    /// Shared relative-path safety or component bounds rejected the path.
    #[error(transparent)]
    RelativePath(#[from] ContractError),
}

/// A bounded NFC path for a future folder-sync protocol.
///
/// Construction validates only protocol syntax. A later filesystem apply must
/// still probe the actual target's name and collision behavior, preserve the
/// source spelling when reporting an unsupported path, and use no-clobber
/// creation. No platform-specific capability is inferred here.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SyncPath(RelativePath);

impl SyncPath {
    /// Validates an authenticated wire spelling without changing its bytes.
    ///
    /// Non-NFC input is rejected because rewriting a signed path would change
    /// the authenticated value.
    pub fn from_wire(value: impl AsRef<str>) -> Result<Self, SyncPathError> {
        let value = value.as_ref();
        check_input_bound(value)?;
        if !is_nfc(value) {
            return Err(SyncPathError::NonCanonicalUnicode);
        }
        Self::from_canonical(value.to_owned())
    }

    /// Normalizes a locally observed UTF-8 spelling to NFC, then validates it.
    ///
    /// The unnormalized input is bounded before normalization, and all byte,
    /// component, and depth limits are checked again on the normalized result.
    /// Target-specific unsupported characters are not renamed or removed.
    pub fn from_local(value: impl AsRef<str>) -> Result<Self, SyncPathError> {
        let value = value.as_ref();
        check_input_bound(value)?;
        let normalized: String = value.nfc().collect();
        Self::from_canonical(normalized)
    }

    fn from_wire_owned(value: String) -> Result<Self, SyncPathError> {
        check_input_bound(&value)?;
        if !is_nfc(&value) {
            return Err(SyncPathError::NonCanonicalUnicode);
        }
        Self::from_canonical(value)
    }

    fn from_canonical(value: String) -> Result<Self, SyncPathError> {
        check_input_bound(&value)?;
        let relative = RelativePath::new(value)?;
        if relative.components().count() > MAX_SYNC_PATH_DEPTH {
            return Err(SyncPathError::TooDeep);
        }
        if relative.as_str().chars().any(char::is_control) {
            return Err(SyncPathError::ControlCharacter);
        }
        Ok(Self(relative))
    }

    /// Returns the canonical slash-separated path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Iterates canonical validated path components.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.components()
    }

    /// Builds a conservative Unicode canonical-caseless comparison hint.
    ///
    /// Equality suggests a possible collision and requires an actual target
    /// probe. Inequality is not permission to overwrite anything. The result
    /// follows Unicode canonical caseless matching using pinned normalization
    /// and case-fold tables; it does not model APFS, SAF, or another filesystem.
    /// Work begins only from the validated 4096-byte path, and the pinned
    /// Unicode decomposition and case-fold tables have finite expansion.
    #[must_use]
    pub fn portable_collision_key(&self) -> PortableCollisionKey {
        let value = self
            .as_str()
            .chars()
            .nfd()
            .default_case_fold()
            .nfd()
            .collect();
        PortableCollisionKey(value)
    }
}

fn check_input_bound(value: &str) -> Result<(), SyncPathError> {
    if value.len() > MAX_SYNC_PATH_BYTES {
        return Err(SyncPathError::TooLong);
    }
    Ok(())
}

impl fmt::Display for SyncPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SyncPath {
    type Err = SyncPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_wire(value)
    }
}

impl AsRef<str> for SyncPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Wire deserialization validates the strict constructor and reuses an owned
/// decoded string. The enclosing transport must still bound the complete wire
/// frame before asking Serde to decode it.
impl<'de> Deserialize<'de> for SyncPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(SyncPathVisitor)
    }
}

struct SyncPathVisitor;

impl Visitor<'_> for SyncPathVisitor {
    type Value = SyncPath;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded NFC relative sync path")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        SyncPath::from_wire(value).map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        SyncPath::from_wire_owned(value).map_err(E::custom)
    }
}

/// A deterministic conservative key for finding possible name collisions.
///
/// This key is only a comparison hint. Filesystem apply must still perform an
/// exact capability and collision probe with no-clobber creation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PortableCollisionKey(String);

impl PortableCollisionKey {
    /// Returns the canonical-caseless key bytes as UTF-8.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str) -> PortableCollisionKey {
        SyncPath::from_local(value)
            .expect("valid local path")
            .portable_collision_key()
    }

    #[test]
    fn wire_rejects_non_nfc_while_local_input_normalizes() {
        let decomposed = "photos/cafe\u{301}.jpg";
        let composed = "photos/café.jpg";
        assert_eq!(
            SyncPath::from_wire(decomposed),
            Err(SyncPathError::NonCanonicalUnicode)
        );
        assert_eq!(
            SyncPath::from_local(decomposed).expect("local normalization"),
            SyncPath::from_wire(composed).expect("canonical wire path")
        );
    }

    #[test]
    fn canonical_caseless_keys_cover_unicode_case_equivalence() {
        assert_eq!(key("Mixed/Case.TXT"), key("mixed/case.txt"));
        assert_eq!(key("Straße.txt"), key("STRASSE.TXT"));
        assert_eq!(key("greek/ΟΣ.txt"), key("GREEK/ος.TXT"));
        assert_eq!(key("greek/οσ.txt"), key("GREEK/ος.TXT"));
        assert_eq!(key("café.txt"), key("cafe\u{301}.txt"));
        assert_ne!(key("résumé.txt"), key("resume.txt"));
    }

    #[test]
    fn accepts_ordinary_non_ascii_and_does_not_invent_platform_rules() {
        for value in [
            "資料/写真.jpg",
            "mañana/Δοκιμή.txt",
            "legal-on-some-targets/name:part",
            ".covalent-internal/user-file",
        ] {
            assert_eq!(
                SyncPath::from_wire(value)
                    .expect("protocol-safe path")
                    .as_str(),
                value
            );
        }
    }

    #[test]
    fn delegates_relative_path_safety_and_rejects_controls() {
        for invalid in [
            "",
            "/absolute",
            "../secret",
            "safe/../secret",
            "safe//file",
            "safe/./file",
            "safe\\file",
            "safe\0file",
        ] {
            assert!(
                matches!(
                    SyncPath::from_wire(invalid),
                    Err(SyncPathError::RelativePath(_))
                ),
                "accepted {invalid:?}"
            );
        }
        assert_eq!(
            SyncPath::from_wire("safe/line\nfeed"),
            Err(SyncPathError::ControlCharacter)
        );
        assert_eq!(
            SyncPath::from_wire("safe/next\u{0085}line"),
            Err(SyncPathError::ControlCharacter)
        );
    }

    #[test]
    fn enforces_component_depth_and_total_byte_bounds() {
        assert!(SyncPath::from_wire("a".repeat(MAX_SYNC_PATH_COMPONENT_BYTES)).is_ok());
        assert!(matches!(
            SyncPath::from_wire("a".repeat(MAX_SYNC_PATH_COMPONENT_BYTES + 1)),
            Err(SyncPathError::RelativePath(_))
        ));

        let depth_128 = vec!["a"; MAX_SYNC_PATH_DEPTH].join("/");
        let depth_129 = vec!["a"; MAX_SYNC_PATH_DEPTH + 1].join("/");
        assert!(SyncPath::from_wire(depth_128).is_ok());
        assert_eq!(SyncPath::from_wire(depth_129), Err(SyncPathError::TooDeep));

        let exact = vec!["a".repeat(240); 17].join("/");
        assert_eq!(exact.len(), MAX_SYNC_PATH_BYTES);
        assert!(SyncPath::from_wire(exact.clone()).is_ok());
        let over = format!("{}a", exact);
        assert_eq!(over.len(), MAX_SYNC_PATH_BYTES + 1);
        assert_eq!(SyncPath::from_wire(over), Err(SyncPathError::TooLong));
    }

    #[test]
    fn local_normalization_is_bounded_before_and_after_work() {
        let oversized_non_nfc = "e\u{301}".repeat(1_400);
        assert!(oversized_non_nfc.len() > MAX_SYNC_PATH_BYTES);
        assert_eq!(
            SyncPath::from_local(oversized_non_nfc),
            Err(SyncPathError::TooLong)
        );

        let expands_under_nfc = "\u{0344}".repeat(MAX_SYNC_PATH_BYTES / 2);
        assert_eq!(expands_under_nfc.len(), MAX_SYNC_PATH_BYTES);
        assert_eq!(
            SyncPath::from_local(expands_under_nfc),
            Err(SyncPathError::TooLong)
        );

        let component_expands_past_limit = "\u{0344}".repeat(64);
        assert_eq!(component_expands_past_limit.len(), 128);
        assert!(matches!(
            SyncPath::from_local(component_expands_past_limit),
            Err(SyncPathError::RelativePath(_))
        ));
    }

    #[test]
    fn serde_uses_the_strict_wire_constructor() {
        let canonical = SyncPath::from_wire("café/file.txt").expect("canonical");
        let encoded = serde_json::to_string(&canonical).expect("serialize");
        assert_eq!(
            serde_json::from_str::<SyncPath>(&encoded).expect("deserialize"),
            canonical
        );
        assert!(serde_json::from_str::<SyncPath>(r#""cafe\u0301/file.txt""#).is_err());
    }

    #[test]
    fn dependency_table_versions_are_reviewed_and_pinned() {
        assert_eq!(caseless::UNICODE_VERSION, (16, 0, 0));
        assert_eq!(unicode_normalization::UNICODE_VERSION, (17, 0, 0));
    }
}
