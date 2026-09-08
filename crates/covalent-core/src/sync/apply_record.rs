//! Canonical private write-ahead records for Unix folder application.
//!
//! These records describe local transaction evidence only. Decoding a record
//! does not admit an operation, verify retained content, authorize a root, or
//! establish that any filesystem mutation occurred.

use std::fmt;
use std::str::FromStr as _;

use covalent_protocol::DeviceId;
use rand_core::{OsRng, RngCore as _};
use thiserror::Error;
use zeroize::Zeroizing;

use super::body::{EntryValue, FileContent};
use super::log_frame::MAX_LOG_PLAINTEXT_BYTES;
use super::operation::OperationDigest;
use super::path::{MAX_SYNC_PATH_BYTES, SyncPath};
use super::register::OpId;

const MAGIC: &[u8; 8] = b"COVSAP01";
const WIRE_VERSION: u16 = 1;
const FLAGS: u8 = 0;
const TRANSACTION_BYTES: usize = 32;
const DIGEST_BYTES: usize = 32;
const UUID_TEXT_BYTES: usize = 36;
const STAGE_RANDOM_BYTES: usize = 16;
const STAGE_HEX_BYTES: usize = STAGE_RANDOM_BYTES * 2;
const STAGE_PREFIX: &str = ".covalent-stage-";
const STAGE_NAME_BYTES: usize = STAGE_PREFIX.len() + STAGE_HEX_BYTES;
const RECORD_DIGEST_DOMAIN: &[u8] = b"covalent/sync-apply-record/v1\0";

const INTENT_TAG: u8 = 1;
const STAGE_READY_TAG: u8 = 2;
const APPLIED_TAG: u8 = 3;
const CONFLICT_TAG: u8 = 4;
const UNSUPPORTED_TAG: u8 = 5;

const FILE_TAG: u8 = 1;
const DIRECTORY_TAG: u8 = 2;
const ABSENT_TAG: u8 = 1;
const EXISTING_DIRECTORY_TAG: u8 = 2;

/// A random identifier for one local apply attempt.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ApplyTransactionId([u8; TRANSACTION_BYTES]);

impl ApplyTransactionId {
    /// Generates a transaction identifier using the operating-system RNG.
    pub fn random() -> Result<Self, ApplyRecordError> {
        Self::random_with(|bytes| {
            OsRng
                .try_fill_bytes(bytes)
                .map_err(|_| ApplyRecordError::EntropyUnavailable)
        })
    }

    fn random_with(
        fill: impl FnOnce(&mut [u8; TRANSACTION_BYTES]) -> Result<(), ApplyRecordError>,
    ) -> Result<Self, ApplyRecordError> {
        let mut bytes = [0_u8; TRANSACTION_BYTES];
        fill(&mut bytes)?;
        Self::from_bytes(bytes).map_err(|_| ApplyRecordError::EntropyUnavailable)
    }

    /// Constructs an identifier from already generated random bytes.
    pub fn from_bytes(bytes: [u8; TRANSACTION_BYTES]) -> Result<Self, ApplyRecordError> {
        if bytes == [0; TRANSACTION_BYTES] {
            return Err(ApplyRecordError::InvalidTransaction);
        }
        Ok(Self(bytes))
    }

    /// Returns the fixed transaction bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; TRANSACTION_BYTES] {
        self.0
    }
}

impl fmt::Debug for ApplyTransactionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApplyTransactionId([redacted])")
    }
}

/// A domain-separated commitment to one complete canonical apply record.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ApplyRecordDigest([u8; DIGEST_BYTES]);

impl ApplyRecordDigest {
    /// Reconstructs a digest carried by a later apply record.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

impl fmt::Debug for ApplyRecordDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApplyRecordDigest([redacted])")
    }
}

/// A descriptor identity captured as a device and inode pair.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryIdentity {
    device: u64,
    inode: u64,
}

impl EntryIdentity {
    /// Constructs an exact local descriptor identity.
    #[must_use]
    pub const fn new(device: u64, inode: u64) -> Self {
        Self { device, inode }
    }

    /// Returns the filesystem device number.
    #[must_use]
    pub const fn device(self) -> u64 {
        self.device
    }

    /// Returns the filesystem inode number.
    #[must_use]
    pub const fn inode(self) -> u64 {
        self.inode
    }
}

impl fmt::Debug for EntryIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EntryIdentity([redacted])")
    }
}

/// The exact admitted operation referenced by a private apply transition.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct OperationBinding {
    id: OpId,
    digest: [u8; DIGEST_BYTES],
}

impl OperationBinding {
    /// Binds an admitted dot to the digest of its canonical signed record.
    #[must_use]
    pub const fn new(id: OpId, digest: OperationDigest) -> Self {
        Self {
            id,
            digest: digest.to_bytes(),
        }
    }

    /// Returns the admitted folder-global operation identity.
    #[must_use]
    pub const fn id(self) -> OpId {
        self.id
    }

    /// Returns the admitted signed-record digest bytes.
    #[must_use]
    pub const fn digest_bytes(self) -> [u8; DIGEST_BYTES] {
        self.digest
    }

    /// Tests an operation digest without constructing an authorization claim.
    #[must_use]
    pub fn matches_digest(self, digest: OperationDigest) -> bool {
        self.digest == digest.to_bytes()
    }

    fn from_parts(id: OpId, digest: [u8; DIGEST_BYTES]) -> Self {
        Self { id, digest }
    }
}

impl fmt::Debug for OperationBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OperationBinding([redacted])")
    }
}

/// A journal-selected internal sibling name.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StageName(String);

impl StageName {
    /// Encodes random bytes as the one canonical internal stage spelling.
    #[must_use]
    pub fn from_random_bytes(random: [u8; STAGE_RANDOM_BYTES]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut name = String::with_capacity(STAGE_NAME_BYTES);
        name.push_str(STAGE_PREFIX);
        for byte in random {
            name.push(char::from(HEX[usize::from(byte >> 4)]));
            name.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Self(name)
    }

    /// Parses the exact lowercase canonical internal spelling.
    pub fn parse(value: &str) -> Result<Self, ApplyRecordError> {
        if value.len() != STAGE_NAME_BYTES || !value.starts_with(STAGE_PREFIX) {
            return Err(ApplyRecordError::InvalidStageName);
        }
        if !value.as_bytes()[STAGE_PREFIX.len()..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(ApplyRecordError::InvalidStageName);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact journal-owned stage spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StageName([redacted])")
    }
}

/// The only mutation actions supported by the create-only journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ApplyAction {
    /// Create an absent file or directory using a no-clobber stage promotion.
    Create = 1,
    /// Verify and durably acknowledge an already present directory.
    EnsureExisting = 2,
}

/// The exact target state observed before an intent became durable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpectedTarget {
    /// No exact or portable-colliding target entry existed.
    Absent,
    /// The desired directory already existed with this exact identity.
    Directory(EntryIdentity),
}

/// A fixed conflict classification; no rejected path or filesystem text is retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ApplyConflictReason {
    /// The target name was already occupied.
    TargetExists = 1,
    /// The target changed after the durable precondition was recorded.
    TargetChanged = 2,
    /// Another spelling has the same portable collision key.
    PortableNameCollision = 3,
    /// A required parent directory was absent.
    ParentMissing = 4,
    /// A parent directory identity changed during traversal.
    ParentChanged = 5,
    /// The authorized root identity changed.
    RootChanged = 6,
    /// A stage recorded as ready was absent during reconciliation.
    StageMissing = 7,
    /// The stage no longer had its recorded identity or content.
    StageChanged = 8,
    /// The promoted final entry no longer had the recorded identity or content.
    FinalChanged = 9,
    /// The admitted operation ceased to be the sole active projection value.
    ProjectionChanged = 10,
    /// More than one causal value remained active for the path.
    MultipleActiveValues = 11,
    /// Filesystem state could not be attributed to a recorded identity.
    UnknownObject = 12,
    /// Verified bytes or supported metadata did not match the desired value.
    ContentMismatch = 13,
}

/// Short compatibility name used by the Unix apply adapter.
pub type ConflictReason = ApplyConflictReason;

/// A fixed reason why the create-only adapter cannot apply an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ApplyUnsupportedReason {
    /// The operation requests deletion.
    Tombstone = 1,
    /// Applying the value would require replacement.
    Replace = 2,
    /// Applying the value would change an incumbent entry's type.
    TypeChange = 3,
    /// A symlink was encountered where following it is forbidden.
    Symlink = 4,
    /// A local directory entry cannot be represented as canonical UTF-8 NFC.
    NonCanonicalLocalName = 5,
    /// The target entry has an unsupported filesystem kind.
    SpecialFile = 6,
    /// The requested executable mode cannot be represented safely.
    ExecutableMode = 7,
    /// A bounded directory scan could not cover the complete parent.
    DirectoryScanLimit = 8,
}

/// Short compatibility name used by the Unix apply adapter.
pub type UnsupportedReason = ApplyUnsupportedReason;

/// A durable pre-mutation write-ahead intent.
#[derive(Clone, Eq, PartialEq)]
pub struct ApplyIntent {
    transaction: ApplyTransactionId,
    operation: OperationBinding,
    root: EntryIdentity,
    path: SyncPath,
    desired: EntryValue,
    action: ApplyAction,
    expected: ExpectedTarget,
    stage_name: Option<StageName>,
}

impl ApplyIntent {
    /// Constructs a semantically valid create-only intent.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        transaction: ApplyTransactionId,
        operation: OperationBinding,
        root: EntryIdentity,
        path: SyncPath,
        desired: EntryValue,
        action: ApplyAction,
        expected: ExpectedTarget,
        stage_name: Option<StageName>,
    ) -> Result<Self, ApplyRecordError> {
        validate_desired(desired)?;
        let valid_shape = match (action, expected, &stage_name) {
            (ApplyAction::Create, ExpectedTarget::Absent, Some(_)) => true,
            (ApplyAction::EnsureExisting, ExpectedTarget::Directory(_), None) => {
                matches!(desired, EntryValue::Directory)
            }
            _ => false,
        };
        if !valid_shape {
            return Err(ApplyRecordError::InvalidShape);
        }
        Ok(Self {
            transaction,
            operation,
            root,
            path,
            desired,
            action,
            expected,
            stage_name,
        })
    }

    /// Returns this local transaction identifier.
    #[must_use]
    pub const fn transaction(&self) -> ApplyTransactionId {
        self.transaction
    }
    /// Returns the exact admitted operation binding.
    #[must_use]
    pub const fn operation(&self) -> OperationBinding {
        self.operation
    }
    /// Returns the authorized-root descriptor identity.
    #[must_use]
    pub const fn root(&self) -> EntryIdentity {
        self.root
    }
    /// Returns the authorized-root descriptor identity.
    #[must_use]
    pub const fn root_identity(&self) -> EntryIdentity {
        self.root
    }
    /// Returns the canonical relative target path.
    #[must_use]
    pub const fn path(&self) -> &SyncPath {
        &self.path
    }
    /// Returns the desired file or directory value.
    #[must_use]
    pub const fn desired(&self) -> EntryValue {
        self.desired
    }
    /// Returns the selected create-only action.
    #[must_use]
    pub const fn action(&self) -> ApplyAction {
        self.action
    }
    /// Returns the exact observed precondition.
    #[must_use]
    pub const fn expected(&self) -> ExpectedTarget {
        self.expected
    }
    /// Returns the exact observed precondition.
    #[must_use]
    pub const fn expected_target(&self) -> ExpectedTarget {
        self.expected
    }
    /// Borrows the journal-selected stage name, if this is a create.
    #[must_use]
    pub fn stage_name(&self) -> Option<&StageName> {
        self.stage_name.as_ref()
    }
}

impl fmt::Debug for ApplyIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApplyIntent([redacted])")
    }
}

/// Durable evidence that the exact journal-owned stage was fully verified.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ApplyStageReady {
    transaction: ApplyTransactionId,
    intent_digest: ApplyRecordDigest,
    operation: OperationBinding,
    stage_identity: EntryIdentity,
    desired: EntryValue,
}

impl ApplyStageReady {
    /// Constructs a stage-ready transition for the exact intent and stage inode.
    pub fn new(
        transaction: ApplyTransactionId,
        intent_digest: ApplyRecordDigest,
        operation: OperationBinding,
        stage_identity: EntryIdentity,
        desired: EntryValue,
    ) -> Result<Self, ApplyRecordError> {
        validate_desired(desired)?;
        Ok(Self {
            transaction,
            intent_digest,
            operation,
            stage_identity,
            desired,
        })
    }

    /// Returns this local transaction identifier.
    #[must_use]
    pub const fn transaction(&self) -> ApplyTransactionId {
        self.transaction
    }
    /// Returns the digest of the exact durable intent.
    #[must_use]
    pub const fn intent_digest(&self) -> ApplyRecordDigest {
        self.intent_digest
    }
    /// Returns the exact admitted operation binding.
    #[must_use]
    pub const fn operation(&self) -> OperationBinding {
        self.operation
    }
    /// Returns the exact verified stage identity.
    #[must_use]
    pub const fn stage_identity(&self) -> EntryIdentity {
        self.stage_identity
    }
    /// Returns the desired value verified in the stage.
    #[must_use]
    pub const fn desired(&self) -> EntryValue {
        self.desired
    }
}

impl fmt::Debug for ApplyStageReady {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApplyStageReady([redacted])")
    }
}

/// The durable success receipt for one exact local apply transaction.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ApplyApplied {
    transaction: ApplyTransactionId,
    intent_digest: ApplyRecordDigest,
    operation: OperationBinding,
    target_identity: EntryIdentity,
    desired: EntryValue,
}

impl ApplyApplied {
    /// Constructs a success receipt after final verification and syncing.
    pub fn new(
        transaction: ApplyTransactionId,
        intent_digest: ApplyRecordDigest,
        operation: OperationBinding,
        target_identity: EntryIdentity,
        desired: EntryValue,
    ) -> Result<Self, ApplyRecordError> {
        validate_desired(desired)?;
        Ok(Self {
            transaction,
            intent_digest,
            operation,
            target_identity,
            desired,
        })
    }

    /// Returns this local transaction identifier.
    #[must_use]
    pub const fn transaction(&self) -> ApplyTransactionId {
        self.transaction
    }
    /// Returns the digest of the exact durable intent.
    #[must_use]
    pub const fn intent_digest(&self) -> ApplyRecordDigest {
        self.intent_digest
    }
    /// Returns the exact admitted operation binding.
    #[must_use]
    pub const fn operation(&self) -> OperationBinding {
        self.operation
    }
    /// Returns the exact verified final target identity.
    #[must_use]
    pub const fn target_identity(&self) -> EntryIdentity {
        self.target_identity
    }
    /// Returns the value verified at the final target.
    #[must_use]
    pub const fn desired(&self) -> EntryValue {
        self.desired
    }
}

impl fmt::Debug for ApplyApplied {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApplyApplied([redacted])")
    }
}

/// A durable visible conflict that does not claim application success.
#[derive(Clone, Eq, PartialEq)]
pub struct ApplyConflict {
    transaction: ApplyTransactionId,
    intent_digest: Option<ApplyRecordDigest>,
    operation: OperationBinding,
    path: SyncPath,
    reason: ApplyConflictReason,
    observed_identity: Option<EntryIdentity>,
}

impl ApplyConflict {
    /// Constructs a fixed conflict record without retaining diagnostic text.
    #[must_use]
    pub const fn new(
        transaction: ApplyTransactionId,
        intent_digest: Option<ApplyRecordDigest>,
        operation: OperationBinding,
        path: SyncPath,
        reason: ApplyConflictReason,
        observed_identity: Option<EntryIdentity>,
    ) -> Self {
        Self {
            transaction,
            intent_digest,
            operation,
            path,
            reason,
            observed_identity,
        }
    }

    /// Returns this local transaction identifier.
    #[must_use]
    pub const fn transaction(&self) -> ApplyTransactionId {
        self.transaction
    }
    /// Returns the exact intent digest for a post-intent conflict.
    #[must_use]
    pub const fn intent_digest(&self) -> Option<ApplyRecordDigest> {
        self.intent_digest
    }
    /// Returns the exact admitted operation binding.
    #[must_use]
    pub const fn operation(&self) -> OperationBinding {
        self.operation
    }
    /// Returns the canonical target path.
    #[must_use]
    pub const fn path(&self) -> &SyncPath {
        &self.path
    }
    /// Returns the fixed conflict classification.
    #[must_use]
    pub const fn reason(&self) -> ApplyConflictReason {
        self.reason
    }
    /// Returns an exact observed inode identity when one was available.
    #[must_use]
    pub const fn observed_identity(&self) -> Option<EntryIdentity> {
        self.observed_identity
    }
}

impl fmt::Debug for ApplyConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApplyConflict([redacted])")
    }
}

/// A durable visible unsupported result that does not claim application success.
#[derive(Clone, Eq, PartialEq)]
pub struct ApplyUnsupported {
    transaction: ApplyTransactionId,
    operation: OperationBinding,
    path: SyncPath,
    reason: ApplyUnsupportedReason,
}

impl ApplyUnsupported {
    /// Constructs a fixed unsupported record without retaining diagnostic text.
    #[must_use]
    pub const fn new(
        transaction: ApplyTransactionId,
        operation: OperationBinding,
        path: SyncPath,
        reason: ApplyUnsupportedReason,
    ) -> Self {
        Self {
            transaction,
            operation,
            path,
            reason,
        }
    }

    /// Returns this local transaction identifier.
    #[must_use]
    pub const fn transaction(&self) -> ApplyTransactionId {
        self.transaction
    }
    /// Returns the exact admitted operation binding.
    #[must_use]
    pub const fn operation(&self) -> OperationBinding {
        self.operation
    }
    /// Returns the canonical target path.
    #[must_use]
    pub const fn path(&self) -> &SyncPath {
        &self.path
    }
    /// Returns the fixed unsupported classification.
    #[must_use]
    pub const fn reason(&self) -> ApplyUnsupportedReason {
        self.reason
    }
}

impl fmt::Debug for ApplyUnsupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApplyUnsupported([redacted])")
    }
}

/// One canonical private apply write-ahead record.
#[derive(Clone, Eq, PartialEq)]
pub enum ApplyRecord {
    /// Durable pre-mutation intent.
    Intent(ApplyIntent),
    /// Durable verified-stage evidence.
    StageReady(ApplyStageReady),
    /// Durable final success receipt.
    Applied(ApplyApplied),
    /// Durable visible conflict.
    Conflict(ApplyConflict),
    /// Durable unsupported result.
    Unsupported(ApplyUnsupported),
}

impl ApplyRecord {
    /// Encodes exactly one canonical bounded record.
    #[must_use]
    pub fn encode(&self) -> EncodedApplyRecord {
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_SYNC_PATH_BYTES + 256));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&WIRE_VERSION.to_be_bytes());
        bytes.push(FLAGS);
        match self {
            Self::Intent(record) => {
                bytes.push(INTENT_TAG);
                encode_transaction(&mut bytes, record.transaction);
                encode_operation(&mut bytes, record.operation);
                encode_identity(&mut bytes, record.root);
                encode_path(&mut bytes, &record.path);
                encode_value(&mut bytes, record.desired);
                bytes.push(record.action as u8);
                encode_expected(&mut bytes, record.expected);
                encode_stage_name(&mut bytes, record.stage_name.as_ref());
            }
            Self::StageReady(record) => {
                bytes.push(STAGE_READY_TAG);
                encode_transaction(&mut bytes, record.transaction);
                bytes.extend_from_slice(&record.intent_digest.0);
                encode_operation(&mut bytes, record.operation);
                encode_identity(&mut bytes, record.stage_identity);
                encode_value(&mut bytes, record.desired);
            }
            Self::Applied(record) => {
                bytes.push(APPLIED_TAG);
                encode_transaction(&mut bytes, record.transaction);
                bytes.extend_from_slice(&record.intent_digest.0);
                encode_operation(&mut bytes, record.operation);
                encode_identity(&mut bytes, record.target_identity);
                encode_value(&mut bytes, record.desired);
            }
            Self::Conflict(record) => {
                bytes.push(CONFLICT_TAG);
                encode_transaction(&mut bytes, record.transaction);
                encode_optional_digest(&mut bytes, record.intent_digest);
                encode_operation(&mut bytes, record.operation);
                encode_path(&mut bytes, &record.path);
                bytes.push(record.reason as u8);
                encode_optional_identity(&mut bytes, record.observed_identity);
            }
            Self::Unsupported(record) => {
                bytes.push(UNSUPPORTED_TAG);
                encode_transaction(&mut bytes, record.transaction);
                encode_operation(&mut bytes, record.operation);
                encode_path(&mut bytes, &record.path);
                bytes.push(record.reason as u8);
            }
        }
        debug_assert!(bytes.len() <= MAX_LOG_PLAINTEXT_BYTES);
        EncodedApplyRecord(bytes)
    }

    /// Decodes one exact bounded canonical record without rewriting any field.
    pub fn decode(bytes: &[u8]) -> Result<Self, ApplyRecordError> {
        if bytes.len() > MAX_LOG_PLAINTEXT_BYTES {
            return Err(ApplyRecordError::TooLarge);
        }
        let mut cursor = Cursor::new(bytes);
        if cursor.take(MAGIC.len())? != MAGIC {
            return Err(ApplyRecordError::InvalidMagic);
        }
        if cursor.u16()? != WIRE_VERSION {
            return Err(ApplyRecordError::UnsupportedVersion);
        }
        if cursor.u8()? != FLAGS {
            return Err(ApplyRecordError::UnsupportedFlags);
        }
        let tag = cursor.u8()?;
        let record = match tag {
            INTENT_TAG => Self::Intent(ApplyIntent::new(
                cursor.transaction()?,
                cursor.operation()?,
                cursor.identity()?,
                cursor.path()?,
                cursor.value()?,
                cursor.action()?,
                cursor.expected()?,
                cursor.stage_name()?,
            )?),
            STAGE_READY_TAG => Self::StageReady(ApplyStageReady::new(
                cursor.transaction()?,
                cursor.digest()?,
                cursor.operation()?,
                cursor.identity()?,
                cursor.value()?,
            )?),
            APPLIED_TAG => Self::Applied(ApplyApplied::new(
                cursor.transaction()?,
                cursor.digest()?,
                cursor.operation()?,
                cursor.identity()?,
                cursor.value()?,
            )?),
            CONFLICT_TAG => Self::Conflict(ApplyConflict::new(
                cursor.transaction()?,
                cursor.optional_digest()?,
                cursor.operation()?,
                cursor.path()?,
                cursor.conflict_reason()?,
                cursor.optional_identity()?,
            )),
            UNSUPPORTED_TAG => Self::Unsupported(ApplyUnsupported::new(
                cursor.transaction()?,
                cursor.operation()?,
                cursor.path()?,
                cursor.unsupported_reason()?,
            )),
            _ => return Err(ApplyRecordError::UnknownTag),
        };
        if !cursor.is_empty() {
            return Err(ApplyRecordError::TrailingBytes);
        }
        Ok(record)
    }

    /// Returns the domain-separated digest of this canonical record.
    #[must_use]
    pub fn digest(&self) -> ApplyRecordDigest {
        digest_bytes(self.encode().as_bytes())
    }
}

impl fmt::Debug for ApplyRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Intent(_) => "Intent",
            Self::StageReady(_) => "StageReady",
            Self::Applied(_) => "Applied",
            Self::Conflict(_) => "Conflict",
            Self::Unsupported(_) => "Unsupported",
        };
        formatter
            .debug_struct("ApplyRecord")
            .field("kind", &kind)
            .finish_non_exhaustive()
    }
}

/// Owned plaintext that redacts its contents and zeroizes on drop.
pub struct EncodedApplyRecord(Zeroizing<Vec<u8>>);

impl EncodedApplyRecord {
    /// Borrows the complete canonical record for immediate encrypted append.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl fmt::Debug for EncodedApplyRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedApplyRecord")
            .field("length", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Validates canonical bytes and returns their domain-separated record digest.
pub fn record_digest(bytes: &[u8]) -> Result<ApplyRecordDigest, ApplyRecordError> {
    let decoded = ApplyRecord::decode(bytes)?;
    let canonical = decoded.encode();
    if canonical.as_bytes() != bytes {
        return Err(ApplyRecordError::NonCanonical);
    }
    Ok(digest_bytes(bytes))
}

/// A fixed redacted apply-record rejection.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ApplyRecordError {
    /// The operating system could not generate a fresh transaction nonce.
    #[error("apply transaction entropy is unavailable")]
    EntropyUnavailable,
    /// The common encrypted-log plaintext bound was exceeded before parsing.
    #[error("apply record exceeds its byte limit")]
    TooLarge,
    /// The record ended before a declared fixed or bounded field completed.
    #[error("apply record is truncated")]
    Truncated,
    /// Bytes remain after the complete selected record shape.
    #[error("apply record has trailing bytes")]
    TrailingBytes,
    /// The record magic does not select this codec.
    #[error("invalid apply record magic")]
    InvalidMagic,
    /// The record version is not implemented.
    #[error("unsupported apply record version")]
    UnsupportedVersion,
    /// Reserved record flags were set.
    #[error("unsupported apply record flags")]
    UnsupportedFlags,
    /// The closed transition tag was unknown.
    #[error("unknown apply record kind")]
    UnknownTag,
    /// The transaction identifier is the reserved all-zero value.
    #[error("invalid apply transaction identifier")]
    InvalidTransaction,
    /// The operation actor spelling or positive counter was invalid.
    #[error("invalid apply operation binding")]
    InvalidOperation,
    /// The path length or canonical path encoding was invalid.
    #[error("invalid canonical apply path")]
    InvalidPath,
    /// Tombstones are not valid desired states in the create-only journal.
    #[error("unsupported desired apply value")]
    UnsupportedValue,
    /// The action tag was outside the closed create-only set.
    #[error("unknown apply action")]
    InvalidAction,
    /// The expected-target tag was outside the closed set.
    #[error("unknown expected target state")]
    InvalidExpectedTarget,
    /// The stage spelling was not the fixed internal lowercase form.
    #[error("invalid apply stage name")]
    InvalidStageName,
    /// Action, expected state, desired value, and stage presence disagree.
    #[error("invalid apply record field combination")]
    InvalidShape,
    /// A fixed conflict or unsupported reason tag was unknown.
    #[error("unknown apply result reason")]
    InvalidReason,
    /// An optional-field tag was not zero or one.
    #[error("invalid apply optional-field tag")]
    InvalidOptionalTag,
    /// Decoding and re-encoding did not preserve the complete byte sequence.
    #[error("apply record encoding is not canonical")]
    NonCanonical,
}

fn validate_desired(value: EntryValue) -> Result<(), ApplyRecordError> {
    match value {
        EntryValue::File(_) | EntryValue::Directory => Ok(()),
        EntryValue::Tombstone => Err(ApplyRecordError::UnsupportedValue),
    }
}

fn digest_bytes(bytes: &[u8]) -> ApplyRecordDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RECORD_DIGEST_DOMAIN);
    hasher.update(bytes);
    ApplyRecordDigest(*hasher.finalize().as_bytes())
}

fn encode_transaction(bytes: &mut Vec<u8>, transaction: ApplyTransactionId) {
    bytes.extend_from_slice(&transaction.0);
}

fn encode_operation(bytes: &mut Vec<u8>, operation: OperationBinding) {
    let actor = operation.id.actor().to_string();
    debug_assert_eq!(actor.len(), UUID_TEXT_BYTES);
    bytes.extend_from_slice(actor.as_bytes());
    bytes.extend_from_slice(&operation.id.counter().to_be_bytes());
    bytes.extend_from_slice(&operation.digest);
}

fn encode_identity(bytes: &mut Vec<u8>, identity: EntryIdentity) {
    bytes.extend_from_slice(&identity.device.to_be_bytes());
    bytes.extend_from_slice(&identity.inode.to_be_bytes());
}

fn encode_path(bytes: &mut Vec<u8>, path: &SyncPath) {
    bytes.extend_from_slice(&(path.as_str().len() as u16).to_be_bytes());
    bytes.extend_from_slice(path.as_str().as_bytes());
}

fn encode_value(bytes: &mut Vec<u8>, value: EntryValue) {
    match value {
        EntryValue::File(file) => {
            bytes.push(FILE_TAG);
            bytes.push(u8::from(file.executable()));
            bytes.extend_from_slice(&file.byte_length().to_be_bytes());
            bytes.extend_from_slice(&file.digest().to_bytes());
        }
        EntryValue::Directory => bytes.push(DIRECTORY_TAG),
        EntryValue::Tombstone => unreachable!("validated apply records exclude tombstones"),
    }
}

fn encode_expected(bytes: &mut Vec<u8>, expected: ExpectedTarget) {
    match expected {
        ExpectedTarget::Absent => bytes.push(ABSENT_TAG),
        ExpectedTarget::Directory(identity) => {
            bytes.push(EXISTING_DIRECTORY_TAG);
            encode_identity(bytes, identity);
        }
    }
}

fn encode_stage_name(bytes: &mut Vec<u8>, stage: Option<&StageName>) {
    match stage {
        None => bytes.push(0),
        Some(stage) => {
            bytes.push(1);
            bytes.extend_from_slice(stage.as_str().as_bytes());
        }
    }
}

fn encode_optional_digest(bytes: &mut Vec<u8>, digest: Option<ApplyRecordDigest>) {
    match digest {
        None => bytes.push(0),
        Some(digest) => {
            bytes.push(1);
            bytes.extend_from_slice(&digest.0);
        }
    }
}

fn encode_optional_identity(bytes: &mut Vec<u8>, identity: Option<EntryIdentity>) {
    match identity {
        None => bytes.push(0),
        Some(identity) => {
            bytes.push(1);
            encode_identity(bytes, identity);
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ApplyRecordError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(ApplyRecordError::Truncated)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(ApplyRecordError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ApplyRecordError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ApplyRecordError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, ApplyRecordError> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, ApplyRecordError> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, ApplyRecordError> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn transaction(&mut self) -> Result<ApplyTransactionId, ApplyRecordError> {
        ApplyTransactionId::from_bytes(self.array()?)
    }

    fn digest(&mut self) -> Result<ApplyRecordDigest, ApplyRecordError> {
        Ok(ApplyRecordDigest::from_bytes(self.array()?))
    }

    fn operation(&mut self) -> Result<OperationBinding, ApplyRecordError> {
        let actor_bytes = self.take(UUID_TEXT_BYTES)?;
        let actor_text =
            std::str::from_utf8(actor_bytes).map_err(|_| ApplyRecordError::InvalidOperation)?;
        let actor =
            DeviceId::from_str(actor_text).map_err(|_| ApplyRecordError::InvalidOperation)?;
        if actor.to_string().as_bytes() != actor_bytes {
            return Err(ApplyRecordError::InvalidOperation);
        }
        let counter = self.u64()?;
        let id = OpId::new(actor, counter).map_err(|_| ApplyRecordError::InvalidOperation)?;
        Ok(OperationBinding::from_parts(id, self.array()?))
    }

    fn identity(&mut self) -> Result<EntryIdentity, ApplyRecordError> {
        Ok(EntryIdentity::new(self.u64()?, self.u64()?))
    }

    fn path(&mut self) -> Result<SyncPath, ApplyRecordError> {
        let length = usize::from(self.u16()?);
        if length > MAX_SYNC_PATH_BYTES {
            return Err(ApplyRecordError::InvalidPath);
        }
        let text =
            std::str::from_utf8(self.take(length)?).map_err(|_| ApplyRecordError::InvalidPath)?;
        SyncPath::from_wire(text).map_err(|_| ApplyRecordError::InvalidPath)
    }

    fn value(&mut self) -> Result<EntryValue, ApplyRecordError> {
        match self.u8()? {
            FILE_TAG => {
                let executable = match self.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(ApplyRecordError::InvalidShape),
                };
                let length = self.u64()?;
                let digest = super::body::ContentDigest::from_bytes(self.array()?);
                FileContent::new(digest, length, executable)
                    .map(EntryValue::File)
                    .map_err(|_| ApplyRecordError::InvalidShape)
            }
            DIRECTORY_TAG => Ok(EntryValue::Directory),
            _ => Err(ApplyRecordError::UnsupportedValue),
        }
    }

    fn action(&mut self) -> Result<ApplyAction, ApplyRecordError> {
        match self.u8()? {
            1 => Ok(ApplyAction::Create),
            2 => Ok(ApplyAction::EnsureExisting),
            _ => Err(ApplyRecordError::InvalidAction),
        }
    }

    fn expected(&mut self) -> Result<ExpectedTarget, ApplyRecordError> {
        match self.u8()? {
            ABSENT_TAG => Ok(ExpectedTarget::Absent),
            EXISTING_DIRECTORY_TAG => self.identity().map(ExpectedTarget::Directory),
            _ => Err(ApplyRecordError::InvalidExpectedTarget),
        }
    }

    fn stage_name(&mut self) -> Result<Option<StageName>, ApplyRecordError> {
        match self.u8()? {
            0 => Ok(None),
            1 => {
                let text = std::str::from_utf8(self.take(STAGE_NAME_BYTES)?)
                    .map_err(|_| ApplyRecordError::InvalidStageName)?;
                StageName::parse(text).map(Some)
            }
            _ => Err(ApplyRecordError::InvalidOptionalTag),
        }
    }

    fn optional_digest(&mut self) -> Result<Option<ApplyRecordDigest>, ApplyRecordError> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.digest().map(Some),
            _ => Err(ApplyRecordError::InvalidOptionalTag),
        }
    }

    fn optional_identity(&mut self) -> Result<Option<EntryIdentity>, ApplyRecordError> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.identity().map(Some),
            _ => Err(ApplyRecordError::InvalidOptionalTag),
        }
    }

    fn conflict_reason(&mut self) -> Result<ApplyConflictReason, ApplyRecordError> {
        match self.u8()? {
            1 => Ok(ApplyConflictReason::TargetExists),
            2 => Ok(ApplyConflictReason::TargetChanged),
            3 => Ok(ApplyConflictReason::PortableNameCollision),
            4 => Ok(ApplyConflictReason::ParentMissing),
            5 => Ok(ApplyConflictReason::ParentChanged),
            6 => Ok(ApplyConflictReason::RootChanged),
            7 => Ok(ApplyConflictReason::StageMissing),
            8 => Ok(ApplyConflictReason::StageChanged),
            9 => Ok(ApplyConflictReason::FinalChanged),
            10 => Ok(ApplyConflictReason::ProjectionChanged),
            11 => Ok(ApplyConflictReason::MultipleActiveValues),
            12 => Ok(ApplyConflictReason::UnknownObject),
            13 => Ok(ApplyConflictReason::ContentMismatch),
            _ => Err(ApplyRecordError::InvalidReason),
        }
    }

    fn unsupported_reason(&mut self) -> Result<ApplyUnsupportedReason, ApplyRecordError> {
        match self.u8()? {
            1 => Ok(ApplyUnsupportedReason::Tombstone),
            2 => Ok(ApplyUnsupportedReason::Replace),
            3 => Ok(ApplyUnsupportedReason::TypeChange),
            4 => Ok(ApplyUnsupportedReason::Symlink),
            5 => Ok(ApplyUnsupportedReason::NonCanonicalLocalName),
            6 => Ok(ApplyUnsupportedReason::SpecialFile),
            7 => Ok(ApplyUnsupportedReason::ExecutableMode),
            8 => Ok(ApplyUnsupportedReason::DirectoryScanLimit),
            _ => Err(ApplyRecordError::InvalidReason),
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn transaction() -> ApplyTransactionId {
        ApplyTransactionId::from_bytes([7; TRANSACTION_BYTES]).expect("transaction")
    }

    fn operation() -> OperationBinding {
        let id = OpId::new(DeviceId::from_uuid(Uuid::from_u128(17)), 9).expect("op id");
        OperationBinding::from_parts(id, [11; DIGEST_BYTES])
    }

    fn path() -> SyncPath {
        SyncPath::from_wire("private/caf\u{e9}.txt").expect("canonical path")
    }

    fn file() -> EntryValue {
        EntryValue::File(
            FileContent::new(
                super::super::body::ContentDigest::from_bytes(
                    *blake3::hash(b"contents").as_bytes(),
                ),
                8,
                true,
            )
            .expect("file"),
        )
    }

    fn stage() -> StageName {
        StageName::from_random_bytes([0xab; STAGE_RANDOM_BYTES])
    }

    fn records() -> Vec<ApplyRecord> {
        let create = ApplyRecord::Intent(
            ApplyIntent::new(
                transaction(),
                operation(),
                EntryIdentity::new(1, 2),
                path(),
                file(),
                ApplyAction::Create,
                ExpectedTarget::Absent,
                Some(stage()),
            )
            .expect("intent"),
        );
        let digest = create.digest();
        vec![
            create,
            ApplyRecord::StageReady(
                ApplyStageReady::new(
                    transaction(),
                    digest,
                    operation(),
                    EntryIdentity::new(3, 4),
                    file(),
                )
                .expect("stage ready"),
            ),
            ApplyRecord::Applied(
                ApplyApplied::new(
                    transaction(),
                    digest,
                    operation(),
                    EntryIdentity::new(3, 4),
                    file(),
                )
                .expect("applied"),
            ),
            ApplyRecord::Conflict(ApplyConflict::new(
                transaction(),
                Some(digest),
                operation(),
                path(),
                ApplyConflictReason::TargetChanged,
                Some(EntryIdentity::new(5, 6)),
            )),
            ApplyRecord::Unsupported(ApplyUnsupported::new(
                transaction(),
                operation(),
                path(),
                ApplyUnsupportedReason::Replace,
            )),
            ApplyRecord::Intent(
                ApplyIntent::new(
                    transaction(),
                    operation(),
                    EntryIdentity::new(1, 2),
                    path(),
                    EntryValue::Directory,
                    ApplyAction::EnsureExisting,
                    ExpectedTarget::Directory(EntryIdentity::new(7, 8)),
                    None,
                )
                .expect("ensure intent"),
            ),
        ]
    }

    #[test]
    fn every_record_round_trips_canonically_within_the_common_log_bound() {
        for record in records() {
            let encoded = record.encode();
            assert!(encoded.as_bytes().len() <= MAX_LOG_PLAINTEXT_BYTES);
            assert_eq!(ApplyRecord::decode(encoded.as_bytes()), Ok(record.clone()));
            assert_eq!(record_digest(encoded.as_bytes()), Ok(record.digest()));
            assert_eq!(record.encode().as_bytes(), encoded.as_bytes());
        }
    }

    #[test]
    fn intent_shape_excludes_tombstones_and_invalid_stage_preconditions() {
        assert_eq!(
            ApplyIntent::new(
                transaction(),
                operation(),
                EntryIdentity::new(1, 2),
                path(),
                EntryValue::Tombstone,
                ApplyAction::Create,
                ExpectedTarget::Absent,
                Some(stage()),
            ),
            Err(ApplyRecordError::UnsupportedValue)
        );
        assert_eq!(
            ApplyIntent::new(
                transaction(),
                operation(),
                EntryIdentity::new(1, 2),
                path(),
                file(),
                ApplyAction::Create,
                ExpectedTarget::Absent,
                None,
            ),
            Err(ApplyRecordError::InvalidShape)
        );
        assert_eq!(
            ApplyIntent::new(
                transaction(),
                operation(),
                EntryIdentity::new(1, 2),
                path(),
                file(),
                ApplyAction::EnsureExisting,
                ExpectedTarget::Directory(EntryIdentity::new(3, 4)),
                None,
            ),
            Err(ApplyRecordError::InvalidShape)
        );
    }

    #[test]
    fn stage_names_have_one_fixed_non_path_derived_spelling() {
        let name = stage();
        assert_eq!(name.as_str().len(), STAGE_NAME_BYTES);
        assert_eq!(StageName::parse(name.as_str()), Ok(name));
        assert_eq!(
            StageName::parse(".covalent-stage-ABABABABABABABABABABABABABABABAB"),
            Err(ApplyRecordError::InvalidStageName)
        );
        assert_eq!(
            StageName::parse("../target"),
            Err(ApplyRecordError::InvalidStageName)
        );
    }

    #[test]
    fn decoder_rejects_truncation_trailing_bytes_and_header_malleability() {
        let encoded = records()[0].encode();
        for end in 0..encoded.as_bytes().len() {
            assert!(ApplyRecord::decode(&encoded.as_bytes()[..end]).is_err());
        }
        let mut trailing = encoded.as_bytes().to_vec();
        trailing.push(0);
        assert_eq!(
            ApplyRecord::decode(&trailing),
            Err(ApplyRecordError::TrailingBytes)
        );

        let mut wrong_version = encoded.as_bytes().to_vec();
        wrong_version[MAGIC.len() + 1] = 2;
        assert_eq!(
            ApplyRecord::decode(&wrong_version),
            Err(ApplyRecordError::UnsupportedVersion)
        );

        let mut flags = encoded.as_bytes().to_vec();
        flags[MAGIC.len() + 2] = 1;
        assert_eq!(
            ApplyRecord::decode(&flags),
            Err(ApplyRecordError::UnsupportedFlags)
        );

        assert_eq!(
            ApplyRecord::decode(&vec![0; MAX_LOG_PLAINTEXT_BYTES + 1]),
            Err(ApplyRecordError::TooLarge)
        );
    }

    #[test]
    fn digest_is_domain_separated_and_commits_to_every_transition_byte() {
        let first = records()[0].clone();
        let encoded = first.encode();
        let plain = blake3::hash(encoded.as_bytes());
        assert_ne!(first.digest().to_bytes(), *plain.as_bytes());

        let mut changed = encoded.as_bytes().to_vec();
        let last = changed.len() - 1;
        changed[last] ^= 1;
        assert_ne!(digest_bytes(&changed), first.digest());
    }

    #[test]
    fn debug_and_errors_do_not_disclose_path_stage_or_content() {
        for record in records() {
            let debug = format!("{record:?} {:?}", record.encode());
            assert!(!debug.contains("private"));
            assert!(!debug.contains("caf"));
            assert!(!debug.contains("covalent-stage"));
            assert!(!debug.contains("contents"));
        }
        let error = format!(
            "{:?} {}",
            ApplyRecordError::InvalidPath,
            ApplyRecordError::InvalidPath
        );
        assert!(!error.contains("private"));
    }

    #[test]
    fn noncanonical_actor_and_optional_tags_are_rejected() {
        let encoded = records()[0].encode();
        let mut upper_actor = encoded.as_bytes().to_vec();
        let actor_offset = MAGIC.len() + 2 + 1 + 1 + TRANSACTION_BYTES;
        upper_actor[actor_offset + 10] = b'A';
        assert_eq!(
            ApplyRecord::decode(&upper_actor),
            Err(ApplyRecordError::InvalidOperation)
        );

        let conflict = records()[3].encode();
        let mut bad_optional = conflict.as_bytes().to_vec();
        bad_optional[MAGIC.len() + 2 + 1 + 1 + TRANSACTION_BYTES] = 2;
        assert_eq!(
            ApplyRecord::decode(&bad_optional),
            Err(ApplyRecordError::InvalidOptionalTag)
        );
    }

    #[test]
    fn transaction_entropy_failure_and_reserved_zero_are_fixed_errors() {
        assert_eq!(
            ApplyTransactionId::random_with(|_| Err(ApplyRecordError::EntropyUnavailable)),
            Err(ApplyRecordError::EntropyUnavailable)
        );
        assert_eq!(
            ApplyTransactionId::random_with(|bytes| {
                *bytes = [0; TRANSACTION_BYTES];
                Ok(())
            }),
            Err(ApplyRecordError::EntropyUnavailable)
        );
        let generated = ApplyTransactionId::random_with(|bytes| {
            *bytes = [7; TRANSACTION_BYTES];
            Ok(())
        })
        .unwrap();
        assert_eq!(generated.to_bytes(), [7; TRANSACTION_BYTES]);
    }
}
