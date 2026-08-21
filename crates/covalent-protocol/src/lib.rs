//! Versioned Covalent wire and persisted contract types.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// First pre-stable Covalent protocol version.
pub const PROTOCOL_VERSION: u16 = 1;
/// Current safe settings export schema.
pub const SETTINGS_SCHEMA_VERSION: u16 = 1;
/// Maximum protocol frame accepted before allocation.
pub const MAX_FRAME_BYTES: usize = 8 * 1_024 * 1_024;
/// Maximum entries accepted in one manifest.
pub const MAX_MANIFEST_ENTRIES: usize = 1_000_000;
/// Largest plaintext chunk accepted by the version-1 contract.
pub const MAX_CHUNK_PLAINTEXT_BYTES: u32 = 8 * 1_024 * 1_024;
/// Largest remembered-backup collection accepted in a settings import.
pub const MAX_REMEMBERED_BACKUPS: usize = 100_000;

const fn protocol_version_one() -> u16 {
    PROTOCOL_VERSION
}

const fn is_false(value: &bool) -> bool {
    !*value
}

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

impl FromStr for DeviceId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
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

    /// Constructs an identifier from a known UUID.
    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
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

impl FromStr for BackupId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
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

/// Authoritative daemon view of one remembered backup and its latest local snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupSummary {
    /// Stable logical backup identifier.
    pub backup_id: BackupId,
    /// User-facing backup name.
    pub name: String,
    /// Device that owns the backup definition.
    pub owner_device_id: DeviceId,
    /// Latest committed snapshot, absent for an imported descriptor with no local snapshot.
    pub latest_snapshot_id: Option<String>,
    /// Commit time of the latest local snapshot.
    pub latest_committed_at_unix_ms: Option<u64>,
    /// Number of immutable snapshots currently retained by this node.
    pub snapshot_count: u64,
    /// Exact providers explicitly selected for the latest snapshot.
    pub selected_provider_ids: BTreeSet<DeviceId>,
}

/// Stable machine-readable API error shared by every client.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiErrorBody {
    /// Contract version used to interpret this error.
    pub protocol_version: u16,
    /// Stable snake-case error identifier.
    pub code: String,
    /// Safe user-facing explanation without secrets or filesystem details.
    pub message: String,
    /// Whether retrying the unchanged request may succeed later.
    pub retryable: bool,
}

/// Transfer category used by progress and event contracts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferKind {
    /// Source scan, encryption, and replication.
    Backup,
    /// Authenticated verification and optional repair.
    Verification,
    /// Root-confined restore or streamed archive export.
    Restore,
}

/// Observable transfer lifecycle shared by native and web clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// Accepted but not yet executing.
    Queued,
    /// Work is actively executing.
    Running,
    /// Durable checkpoint is retained for explicit resume.
    Paused,
    /// Work completed successfully.
    Completed,
    /// Work stopped after a safe failure.
    Failed,
    /// Work was explicitly cancelled and its checkpoint discarded.
    Cancelled,
}

/// Versioned bounded progress snapshot suitable for polling or event payloads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferProgress {
    /// Active protocol version.
    pub protocol_version: u16,
    /// Stable resumable job identifier.
    pub job_id: String,
    /// Operation category.
    pub kind: TransferKind,
    /// Current lifecycle state.
    pub state: TransferState,
    /// Plaintext or output bytes durably processed so far.
    pub completed_bytes: u64,
    /// Known total byte count, absent while discovery is incomplete.
    pub total_bytes: Option<u64>,
    /// Number of filesystem entries durably processed.
    pub completed_entries: u64,
    /// Safe short status text.
    pub message: String,
}

/// Event category emitted by a node without embedding private payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeEventKind {
    /// A transfer changed lifecycle state or progress.
    TransferChanged,
    /// A peer or provider connection changed.
    PeerChanged,
    /// Local non-secret settings changed.
    SettingsChanged,
}

/// Ordered versioned node event contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeEvent {
    /// Active protocol version.
    pub protocol_version: u16,
    /// Monotonic sequence within one node process.
    pub sequence: u64,
    /// Event creation time.
    pub occurred_at_unix_ms: u64,
    /// Stable event category.
    pub kind: NodeEventKind,
    /// Related resumable job when applicable.
    pub job_id: Option<String>,
    /// Safe short status text.
    pub message: String,
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
        let settings = Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            device_name,
            lan_discovery_enabled,
            remembered_backups,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Validates a decoded import before it reaches local state.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(ContractError::UnsupportedSettingsSchema(
                self.schema_version,
            ));
        }
        validate_device_name(&self.device_name)?;
        if self.remembered_backups.len() > MAX_REMEMBERED_BACKUPS {
            return Err(ContractError::SettingsTooLarge);
        }
        let mut backup_ids = BTreeSet::new();
        for backup in &self.remembered_backups {
            validate_backup_name(&backup.name)?;
            if !backup_ids.insert(backup.backup_id) {
                return Err(ContractError::DuplicateRememberedBackup);
            }
        }
        Ok(())
    }
}

fn validate_device_name(value: &str) -> Result<(), ContractError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 80 || trimmed.chars().any(char::is_control) {
        return Err(ContractError::InvalidDeviceName);
    }
    Ok(())
}

fn validate_backup_name(value: &str) -> Result<(), ContractError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 120 || trimmed.chars().any(char::is_control) {
        return Err(ContractError::InvalidBackupName);
    }
    Ok(())
}

/// Transport endpoint and TLS identity bound into a mutually confirmed pairing.
///
/// Discovery may suggest an address, but callers must use this record only after
/// the surrounding pairing transcript has been verified and finalized.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransportBinding {
    /// Device identity that owns the transport endpoint.
    pub peer_id: DeviceId,
    /// User-confirmed device name.
    pub display_name: String,
    /// Canonical numeric socket address, including the peer port.
    pub address: String,
    /// Base64url-encoded DER certificate pinned by the QUIC client.
    pub certificate_der: String,
    /// Lowercase SHA-256 digest of the DER certificate.
    pub certificate_fingerprint: String,
}

/// Provider-issued, backup-scoped reservation required for every remote object write.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StorageLease {
    pub schema_version: u16,
    pub lease_id: String,
    pub peer_device_id: DeviceId,
    pub provider_device_id: DeviceId,
    pub backup_id: BackupId,
    pub max_new_bytes: u64,
    pub max_new_objects: u64,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub nonce: String,
    pub signature: String,
}

/// An expiring, single-use pairing invitation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingInvitation {
    /// Invitation format version.
    pub protocol_version: u16,
    /// Lowest protocol version the inviter will accept.
    #[serde(default = "protocol_version_one")]
    pub minimum_protocol_version: u16,
    /// Inviting device identifier.
    pub inviter_device_id: DeviceId,
    /// Base64url-encoded public signing key.
    pub inviter_public_key: String,
    /// User-visible inviter name bound into the signed invitation.
    #[serde(default)]
    pub inviter_device_name: String,
    /// Opaque random invitation identifier.
    pub invitation_id: String,
    /// Base64url invitation secret used only while the invitation is pending.
    #[serde(default)]
    pub invitation_secret: String,
    /// BLAKE3 commitment to the decoded invitation secret.
    #[serde(default)]
    pub invitation_secret_commitment: String,
    /// Unix timestamp in milliseconds after which pairing must fail.
    pub expires_at_unix_ms: u64,
    /// Candidate connection hints; never trusted as identity.
    pub endpoints: Vec<String>,
    /// Inviter transport identity covered by the invitation signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_binding: Option<TransportBinding>,
    /// Base64url Ed25519 signature over all preceding invitation fields.
    #[serde(default)]
    pub signature: String,
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

/// One client-observed target entry used to compute external restore actions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetInventoryEntry {
    /// Safe path relative to the selected SAF or PowerBox root.
    pub path: RelativePath,
    /// Observed target kind. Symlinks and unsupported kinds are rejected client-side.
    pub kind: EntryKind,
    /// Observed regular-file length, or zero for directories.
    pub length: u64,
    /// Observed modification time when the provider exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at_unix_ms: Option<u64>,
    /// Bounded client/provider identity token used to detect replacement before apply.
    pub identity_token: String,
}

/// Canonical client-owned target inventory supplied to preview generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetInventory {
    pub schema_version: u16,
    /// Stable client-owned identity for the authorized SAF tree or PowerBox root.
    pub root_identity: String,
    pub entry_count: u64,
    pub total_bytes: u64,
    pub inventory_digest: String,
    /// Strictly path-sorted entries used by the engine to compute actions.
    pub entries: Vec<TargetInventoryEntry>,
}

/// Bounded summary of a client-owned restore target inventory.
///
/// The inventory digest is BLAKE3 over canonical JSON entries sorted by relative
/// path. The actions digest is BLAKE3 over the exact ordered restore actions the
/// client previewed against that inventory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetInventoryBinding {
    /// Binding schema; currently one.
    pub schema_version: u16,
    /// Stable client-owned identity for the authorized SAF tree or PowerBox root.
    pub root_identity: String,
    /// Exact number of canonical target entries represented by the digest.
    pub entry_count: u64,
    /// Sum of regular-file lengths with checked arithmetic.
    pub total_bytes: u64,
    /// Lowercase BLAKE3 digest of canonical inventory entries.
    pub inventory_digest: String,
    /// Lowercase BLAKE3 digest of exact client-side conflict actions.
    pub actions_digest: String,
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
    /// Portable metadata restored on a best-effort, platform-safe basis.
    #[serde(default, skip_serializing_if = "EntryMetadata::is_empty")]
    pub metadata: EntryMetadata,
    /// Data extents for sparse files. Empty means dense content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sparse_extents: Vec<SparseExtent>,
}

/// Portable filesystem metadata captured without following links.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntryMetadata {
    /// Last modification timestamp when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at_unix_ms: Option<u64>,
    /// Unix permission bits when captured on a Unix source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_mode: Option<u32>,
    /// True when holes must be reconstructed even if no data extent exists.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sparse: bool,
}

impl EntryMetadata {
    /// Returns whether no portable metadata is present.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.modified_at_unix_ms.is_none() && self.unix_mode.is_none() && !self.sparse
    }
}

/// One logical data extent in a sparse file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SparseExtent {
    /// Logical byte offset in the restored file.
    pub offset: u64,
    /// Number of plaintext data bytes represented by this extent.
    pub length: u64,
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
    /// Durable provider acknowledgements, separate from requested intent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_acknowledgements: BTreeMap<DeviceId, BTreeSet<String>>,
}

impl Manifest {
    /// Validates size, ordering, paths, chunk digests, and explicit placement.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ContractError::UnsupportedProtocol(self.protocol_version));
        }
        if self.entries.len() > MAX_MANIFEST_ENTRIES {
            return Err(ContractError::ManifestTooLarge);
        }
        if self.snapshot_id.is_empty()
            || self.snapshot_id.len() > 128
            || !self
                .snapshot_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || self.replica_intent.selected_providers.len() > 128
        {
            return Err(ContractError::InvalidManifestEntry);
        }
        let mut previous: Option<&RelativePath> = None;
        let mut manifest_locators = BTreeSet::new();
        for entry in &self.entries {
            if previous.is_some_and(|path| path >= &entry.path) {
                return Err(ContractError::ManifestEntriesNotSorted);
            }
            previous = Some(&entry.path);
            if entry.kind == EntryKind::Directory && (entry.length != 0 || !entry.chunks.is_empty())
            {
                return Err(ContractError::InvalidManifestEntry);
            }
            if entry.kind == EntryKind::Directory
                && (entry.metadata.sparse || !entry.sparse_extents.is_empty())
            {
                return Err(ContractError::InvalidManifestEntry);
            }
            let chunk_length: u64 = entry
                .chunks
                .iter()
                .map(|chunk| u64::from(chunk.plaintext_length))
                .sum();
            if entry.kind == EntryKind::File
                && !entry.metadata.sparse
                && entry.sparse_extents.is_empty()
                && chunk_length != entry.length
            {
                return Err(ContractError::InvalidManifestEntry);
            }
            if !entry.metadata.sparse && !entry.sparse_extents.is_empty() {
                return Err(ContractError::InvalidManifestEntry);
            }
            if entry.metadata.sparse {
                let extent_length: u64 = entry
                    .sparse_extents
                    .iter()
                    .map(|extent| extent.length)
                    .sum();
                let mut previous_end = 0_u64;
                if extent_length != chunk_length
                    || entry.sparse_extents.iter().any(|extent| {
                        let invalid = extent.length == 0
                            || extent.offset < previous_end
                            || extent.offset.checked_add(extent.length).is_none()
                            || extent.offset.saturating_add(extent.length) > entry.length;
                        previous_end = extent.offset.saturating_add(extent.length);
                        invalid
                    })
                {
                    return Err(ContractError::InvalidManifestEntry);
                }
            }
            for chunk in &entry.chunks {
                if chunk.plaintext_digest.len() != 64
                    || chunk.opaque_locator.len() != 64
                    || !chunk
                        .plaintext_digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    || !chunk
                        .opaque_locator
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    || chunk
                        .plaintext_digest
                        .bytes()
                        .any(|byte| byte.is_ascii_uppercase())
                    || chunk
                        .opaque_locator
                        .bytes()
                        .any(|byte| byte.is_ascii_uppercase())
                    || chunk.plaintext_length == 0
                    || chunk.plaintext_length > MAX_CHUNK_PLAINTEXT_BYTES
                    || chunk.ciphertext_length != chunk.plaintext_length.saturating_add(16)
                {
                    return Err(ContractError::InvalidChunkReference);
                }
                manifest_locators.insert(chunk.opaque_locator.clone());
            }
        }
        if self
            .provider_acknowledgements
            .keys()
            .any(|provider| !self.replica_intent.contains(provider))
        {
            return Err(ContractError::UnselectedProviderAcknowledgement);
        }
        if self
            .provider_acknowledgements
            .values()
            .flatten()
            .any(|locator| {
                locator.len() != 64
                    || locator
                        .bytes()
                        .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
                    || !manifest_locators.contains(locator)
            })
        {
            return Err(ContractError::InvalidProviderAcknowledgement);
        }
        Ok(())
    }
}

/// Signed, independently encrypted manifest record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestEnvelope {
    /// Envelope format version.
    pub protocol_version: u16,
    /// Backup whose epoch key decrypts this record.
    pub backup_id: BackupId,
    /// Monotonic content-key epoch.
    pub key_epoch: u64,
    /// Fixed cipher suite identifier.
    pub cipher_suite: String,
    /// Base64url XChaCha20 nonce.
    pub nonce: String,
    /// Base64url authenticated ciphertext.
    pub ciphertext: String,
    /// Identity that signed the envelope fields.
    pub signer_device_id: DeviceId,
    /// Base64url Ed25519 signature over canonical envelope signing bytes.
    pub signature: String,
}

/// Roles granted to one explicitly trusted peer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerRole {
    /// May provide encrypted chunk storage.
    StorageProvider,
    /// May read authorized encrypted backup objects.
    BackupReader,
    /// May submit encrypted chunks for explicitly selected backups.
    BackupWriter,
}

/// Persisted peer authorization created only after explicit confirmation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerGrant {
    /// Authorized peer identity.
    pub peer_device_id: DeviceId,
    /// Base64url Ed25519 public key.
    pub public_key: String,
    /// Human-readable name last confirmed by the user.
    pub display_name: String,
    /// Exact allowed roles.
    pub roles: BTreeSet<PeerRole>,
    /// Unix timestamp of explicit confirmation.
    pub confirmed_at_unix_ms: u64,
    /// Whether this grant has been revoked.
    pub revoked: bool,
}

/// Signed monotonic roster shared only among remembered peers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedRoster {
    /// Protocol version.
    pub protocol_version: u16,
    /// Strictly increasing local roster epoch.
    pub epoch: u64,
    /// Digest of the preceding accepted roster, or empty for genesis.
    pub previous_digest: String,
    /// Complete grants, including revocation tombstones.
    pub grants: Vec<PeerGrant>,
    /// Device authorized to sign this epoch.
    pub signer_device_id: DeviceId,
    /// Base64url Ed25519 signature over canonical roster fields.
    pub signature: String,
}

/// Restore behavior when a destination already exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Abort before changing any conflicting entry.
    Fail,
    /// Leave existing entries unchanged.
    Skip,
    /// Atomically replace regular files; never replace directories with files.
    Replace,
    /// Select a deterministic available sibling name.
    Rename,
}

/// Availability of an explicitly selected replica.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicaAvailability {
    /// Every required encrypted object is durably acknowledged.
    Complete,
    /// Some objects are missing.
    Degraded,
    /// Provider is not currently reachable.
    Offline,
    /// Provider returned corrupt or unauthenticated content.
    Corrupt,
    /// Peer has been revoked and may not serve new requests.
    Revoked,
}

/// Protocol negotiation hello authenticated by the paired transport.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerHello {
    /// Stable identity.
    pub device_id: DeviceId,
    /// Minimum accepted protocol version.
    pub minimum_protocol_version: u16,
    /// Maximum accepted protocol version.
    pub maximum_protocol_version: u16,
    /// Random replay-resistant connection nonce.
    pub nonce: String,
    /// Base64url Ed25519 signature over the hello transcript.
    pub signature: String,
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
    /// A remembered backup name is empty, too long, or contains control characters.
    #[error("backup name must be 1-120 printable characters")]
    InvalidBackupName,
    /// A settings import contains too many remembered backups.
    #[error("settings contain too many remembered backups")]
    SettingsTooLarge,
    /// A settings import repeats one logical backup identifier.
    #[error("settings contain duplicate remembered backups")]
    DuplicateRememberedBackup,
    /// The imported settings schema is not supported.
    #[error("unsupported settings schema version {0}")]
    UnsupportedSettingsSchema(u16),
    /// A persisted or wire object uses an unsupported protocol.
    #[error("unsupported protocol version {0}")]
    UnsupportedProtocol(u16),
    /// A manifest exceeds the bounded entry count.
    #[error("manifest exceeds the entry limit")]
    ManifestTooLarge,
    /// Manifest entries must be unique and strictly sorted.
    #[error("manifest entries are not strictly sorted")]
    ManifestEntriesNotSorted,
    /// File lengths, extents, or directory content are inconsistent.
    #[error("manifest entry is internally inconsistent")]
    InvalidManifestEntry,
    /// A chunk digest, locator, or length is malformed.
    #[error("manifest chunk reference is invalid")]
    InvalidChunkReference,
    /// An acknowledgement names a provider the user did not select.
    #[error("manifest acknowledges an unselected provider")]
    UnselectedProviderAcknowledgement,
    /// A provider acknowledgement does not name an object in this manifest.
    #[error("manifest provider acknowledgement is invalid")]
    InvalidProviderAcknowledgement,
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
