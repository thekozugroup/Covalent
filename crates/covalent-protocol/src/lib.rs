//! Versioned Covalent wire and persisted contract types.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// First pre-stable Covalent protocol version.
pub const PROTOCOL_VERSION: u16 = 1;
/// Current safe settings export schema.
pub const SETTINGS_SCHEMA_VERSION: u16 = 1;

/// A stable public device identifier bound to a signing identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(Uuid);

impl DeviceId {
    /// Creates a fresh device identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Constructs an identifier from a known UUID.
    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

impl Default for DeviceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A stable logical backup identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackupId(Uuid);

impl BackupId {
    /// Creates a fresh backup identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for BackupId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BackupId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A normalized protocol path, always relative and slash-separated.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RelativePath(String);

impl RelativePath {
    /// Validates a path without silently normalizing ambiguous input.
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        validate_relative_path(&value)?;
        Ok(Self(value))
    }

    /// Returns the canonical protocol string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Iterates validated path components.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

fn validate_relative_path(value: &str) -> Result<(), ContractError> {
    if value.is_empty() {
        return Err(ContractError::InvalidRelativePath("path is empty"));
    }
    if value.len() > 4_096 {
        return Err(ContractError::InvalidRelativePath(
            "path exceeds 4096 bytes",
        ));
    }
    if value.starts_with('/') {
        return Err(ContractError::InvalidRelativePath(
            "absolute paths are forbidden",
        ));
    }
    if value.contains(['\\', '\0']) {
        return Err(ContractError::InvalidRelativePath(
            "backslash and NUL are forbidden",
        ));
    }
    for component in value.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(ContractError::InvalidRelativePath(
                "empty, dot, and parent components are forbidden",
            ));
        }
        if component.len() > 255 {
            return Err(ContractError::InvalidRelativePath(
                "a component exceeds 255 bytes",
            ));
        }
    }
    Ok(())
}

/// User-selected providers for extra copies. No automatic target count exists.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplicaIntent {
    /// Exact provider identifiers chosen by the user.
    pub selected_providers: BTreeSet<DeviceId>,
}

impl ReplicaIntent {
    /// Captures an explicit selection. An empty selection means local-only backup.
    #[must_use]
    pub fn explicit(selected_providers: impl IntoIterator<Item = DeviceId>) -> Self {
        Self {
            selected_providers: selected_providers.into_iter().collect(),
        }
    }

    /// Returns whether this provider was explicitly selected.
    #[must_use]
    pub fn contains(&self, provider: &DeviceId) -> bool {
        self.selected_providers.contains(provider)
    }
}

/// A remembered backup descriptor safe for normal settings export.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RememberedBackup {
    /// Logical backup identifier.
    pub backup_id: BackupId,
    /// User-facing backup name.
    pub name: String,
    /// Device that owns the backup definition.
    pub owner_device_id: DeviceId,
}

/// Settings deliberately excluding identities, content keys, and access grants.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportedDeviceSettings {
    /// Export schema version.
    pub schema_version: u16,
    /// User-visible device name.
    pub device_name: String,
    /// Whether local multicast discovery is enabled.
    pub lan_discovery_enabled: bool,
    /// Non-secret remembered backup descriptors.
    pub remembered_backups: Vec<RememberedBackup>,
}

impl ExportedDeviceSettings {
    /// Creates and validates an export-safe settings value.
    pub fn new(
        device_name: impl Into<String>,
        lan_discovery_enabled: bool,
        remembered_backups: Vec<RememberedBackup>,
    ) -> Result<Self, ContractError> {
        let device_name = device_name.into();
        validate_device_name(&device_name)?;
        Ok(Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            device_name,
            lan_discovery_enabled,
            remembered_backups,
        })
    }

    /// Validates a decoded import before it reaches local state.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(ContractError::UnsupportedSettingsSchema(
                self.schema_version,
            ));
        }
        validate_device_name(&self.device_name)
    }
}

fn validate_device_name(value: &str) -> Result<(), ContractError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 80 || trimmed.chars().any(char::is_control) {
        return Err(ContractError::InvalidDeviceName);
    }
    Ok(())
}

/// An expiring, single-use pairing invitation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingInvitation {
    /// Invitation format version.
    pub protocol_version: u16,
    /// Inviting device identifier.
    pub inviter_device_id: DeviceId,
    /// Base64url-encoded public signing key.
    pub inviter_public_key: String,
    /// Opaque random invitation identifier.
    pub invitation_id: String,
    /// Unix timestamp in milliseconds after which pairing must fail.
    pub expires_at_unix_ms: u64,
    /// Candidate connection hints; never trusted as identity.
    pub endpoints: Vec<String>,
}

/// A content chunk reference held inside an encrypted manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChunkReference {
    /// BLAKE3 plaintext digest encoded as lowercase hex.
    pub plaintext_digest: String,
    /// Provider-visible keyed opaque locator.
    pub opaque_locator: String,
    /// Plaintext byte count.
    pub plaintext_length: u32,
    /// Authenticated ciphertext byte count.
    pub ciphertext_length: u32,
}

/// Filesystem entry kind represented in a manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Regular file.
    File,
    /// Empty or metadata-bearing directory.
    Directory,
}

/// One safe relative entry inside an encrypted manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestEntry {
    /// Path relative to the authorized source root.
    pub path: RelativePath,
    /// Filesystem entry kind.
    pub kind: EntryKind,
    /// File length, or zero for a directory.
    pub length: u64,
    /// Ordered content chunks; empty for directories and empty files.
    pub chunks: Vec<ChunkReference>,
}

/// Decrypted manifest payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    /// Protocol version used for canonical encoding.
    pub protocol_version: u16,
    /// Logical backup identifier.
    pub backup_id: BackupId,
    /// Monotonic snapshot identifier.
    pub snapshot_id: String,
    /// Creation time as Unix milliseconds.
    pub created_at_unix_ms: u64,
    /// User-selected extra-copy providers.
    pub replica_intent: ReplicaIntent,
    /// Sorted filesystem entries.
    pub entries: Vec<ManifestEntry>,
}

/// Product platform readiness tier exposed in local status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformTier {
    /// macOS, Android, Docker, or Unraid release-blocking surface.
    Tier1,
    /// Supported iOS track that does not block Tier 1 readiness.
    Tier2,
}

/// Non-sensitive daemon status returned to local clients.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeStatus {
    /// User-visible device name.
    pub device_name: String,
    /// Active protocol version.
    pub protocol_version: u16,
    /// Current LAN discovery preference.
    pub lan_discovery: bool,
    /// Platform release tier.
    pub platform_tier: PlatformTier,
    /// Coarse service lifecycle state.
    pub state: String,
}

/// Contract validation error.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ContractError {
    /// A restore or manifest path violates canonical relative-path rules.
    #[error("invalid relative path: {0}")]
    InvalidRelativePath(&'static str),
    /// A device name is empty, too long, or contains control characters.
    #[error("device name must be 1-80 printable characters")]
    InvalidDeviceName,
    /// The imported settings schema is not supported.
    #[error("unsupported settings schema version {0}")]
    UnsupportedSettingsSchema(u16),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_fail_closed() {
        for invalid in [
            "",
            "/etc/passwd",
            "../secret",
            "safe/../secret",
            "safe//file",
            "safe/./file",
            "safe\\file",
            "safe\0file",
        ] {
            assert!(RelativePath::new(invalid).is_err(), "accepted {invalid:?}");
        }
        assert_eq!(
            RelativePath::new("photos/2026/image.jpg")
                .expect("safe path")
                .as_str(),
            "photos/2026/image.jpg"
        );
    }

    #[test]
    fn settings_reject_private_identity_fields() {
        let input = r#"{
            "schemaVersion": 1,
            "deviceName": "Home Mac",
            "lanDiscoveryEnabled": false,
            "rememberedBackups": [],
            "privateIdentityKey": "must-not-import"
        }"#;
        assert!(serde_json::from_str::<ExportedDeviceSettings>(input).is_err());
    }

    #[test]
    fn replicas_are_exactly_the_explicit_selection() {
        let selected = DeviceId::new();
        let not_selected = DeviceId::new();
        let intent = ReplicaIntent::explicit([selected]);
        assert!(intent.contains(&selected));
        assert!(!intent.contains(&not_selected));
    }
}
