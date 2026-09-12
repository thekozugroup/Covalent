//! Domain-separated signed records for explicit two-device folder sharing.
//!
//! These records bind public identities and direct endpoints. They do not grant
//! trust, persist consent, authorize a local path, or start a sync engine. The
//! caller supplies already trusted Covalent identities and owns replay,
//! revocation, durable state and process sequencing.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_protocol::{
    DeviceId, FOLDER_LINK_SCHEMA_VERSION, FOLDER_SHARE_SCHEMA_VERSION, FolderShareAcceptance,
    FolderShareCommit, FolderShareOffer, MAX_FOLDER_SHARE_LABEL_BYTES,
    MAX_FOLDER_SHARE_PAIRING_ID_BYTES, SyncEngineBinding,
};
use rand_core::{OsRng, RngCore as _};
use serde::Serialize;
use uuid::Uuid;

use crate::{DeviceIdentity, PublicIdentity};

const OFFER_SIGNATURE_DOMAIN: &[u8] = b"covalent/folder-share-offer/v1";
const ACCEPTANCE_SIGNATURE_DOMAIN: &[u8] = b"covalent/folder-share-acceptance/v1";
const COMMIT_SIGNATURE_DOMAIN: &[u8] = b"covalent/folder-share-commit/v1";
const OFFER_DIGEST_DOMAIN: &[u8] = b"covalent/folder-share-offer-digest/v1\0";
const ACCEPTANCE_DIGEST_DOMAIN: &[u8] = b"covalent/folder-share-acceptance-digest/v1\0";
const SHARE_NONCE_BYTES: usize = 24;
const ED25519_SIGNATURE_BYTES: usize = 64;
const SHARE_DIGEST_BYTES: usize = 32;
const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1_000;
/// Longest interval during which a new folder invitation may be accepted.
pub const MAX_FOLDER_SHARE_LIFETIME_MS: u64 = 15 * 60 * 1_000;

/// Fixed, redacted failure from signed folder-sharing record processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderSharingError {
    /// A field, schema, identifier, timestamp, or canonical encoding was invalid.
    InvalidRecord,
    /// A public engine identity or endpoint was invalid or owned by another peer.
    InvalidEngineBinding,
    /// The supplied trusted identity was not the record's exact source or target.
    WrongPeer,
    /// A signature could not be authenticated by the expected trusted identity.
    InvalidSignature,
    /// A fresh intake arrived outside the offer's bounded acceptance window.
    NotFresh,
    /// Operating-system entropy was unavailable or returned an unusable value.
    EntropyUnavailable,
    /// A bounded canonical record could not be encoded.
    EncodingFailed,
}

impl fmt::Display for FolderSharingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRecord => "folder share record is invalid",
            Self::InvalidEngineBinding => "folder share engine binding is invalid",
            Self::WrongPeer => "folder share record names another peer",
            Self::InvalidSignature => "folder share signature is invalid",
            Self::NotFresh => "folder share invitation is no longer fresh",
            Self::EntropyUnavailable => "folder share randomness is unavailable",
            Self::EncodingFailed => "folder share record could not be encoded",
        })
    }
}

impl std::error::Error for FolderSharingError {}

/// Create a source-signed, expiring invitation for one exact target and folder.
#[allow(clippy::too_many_arguments)]
pub fn create_folder_share_offer(
    source: &DeviceIdentity,
    target_device_id: DeviceId,
    folder_id: Uuid,
    label: &str,
    source_engine: SyncEngineBinding,
    pairing_id: Option<&str>,
    now_unix_ms: u64,
    lifetime_ms: u64,
) -> Result<FolderShareOffer, FolderSharingError> {
    create_folder_share_offer_with_policy(
        source,
        target_device_id,
        folder_id,
        label,
        source_engine,
        pairing_id,
        now_unix_ms,
        lifetime_ms,
        None,
    )
}

/// Create an offer whose signed policy fixes one-way direction and deletion choices.
#[allow(clippy::too_many_arguments)]
pub fn create_folder_share_offer_with_policy(
    source: &DeviceIdentity,
    target_device_id: DeviceId,
    folder_id: Uuid,
    label: &str,
    source_engine: SyncEngineBinding,
    pairing_id: Option<&str>,
    now_unix_ms: u64,
    lifetime_ms: u64,
    link_policy: Option<covalent_protocol::FolderLinkPolicy>,
) -> Result<FolderShareOffer, FolderSharingError> {
    validate_label(label)?;
    validate_pairing_id(pairing_id)?;
    validate_binding(&source_engine, source.device_id())?;
    if source.device_id() == target_device_id
        || is_nil_device_id(target_device_id)
        || folder_id.is_nil()
        || now_unix_ms == 0
        || lifetime_ms == 0
        || lifetime_ms > MAX_FOLDER_SHARE_LIFETIME_MS
    {
        return Err(FolderSharingError::InvalidRecord);
    }
    let expires_at_unix_ms = now_unix_ms
        .checked_add(lifetime_ms)
        .ok_or(FolderSharingError::InvalidRecord)?;
    let mut random = [0_u8; 16 + SHARE_NONCE_BYTES];
    OsRng
        .try_fill_bytes(&mut random)
        .map_err(|_| FolderSharingError::EntropyUnavailable)?;
    let mut offer_id_bytes: [u8; 16] = random[..16]
        .try_into()
        .map_err(|_| FolderSharingError::EntropyUnavailable)?;
    if offer_id_bytes.iter().all(|byte| *byte == 0) || random[16..].iter().all(|byte| *byte == 0) {
        return Err(FolderSharingError::EntropyUnavailable);
    }
    offer_id_bytes[6] = (offer_id_bytes[6] & 0x0f) | 0x40;
    offer_id_bytes[8] = (offer_id_bytes[8] & 0x3f) | 0x80;
    let mut offer = FolderShareOffer {
        schema_version: if link_policy.is_some() {
            FOLDER_LINK_SCHEMA_VERSION
        } else {
            FOLDER_SHARE_SCHEMA_VERSION
        },
        offer_id: Uuid::from_bytes(offer_id_bytes),
        folder_id,
        label: label.to_owned(),
        source_device_id: source.device_id(),
        target_device_id,
        source_engine,
        link_policy,
        pairing_id: pairing_id.map(str::to_owned),
        issued_at_unix_ms: now_unix_ms,
        expires_at_unix_ms,
        nonce: URL_SAFE_NO_PAD.encode(&random[16..]),
        signature: String::new(),
    };
    validate_offer_shape(&offer, false)?;
    offer.signature = source.sign(OFFER_SIGNATURE_DOMAIN, &offer_signing_bytes(&offer)?);
    validate_offer_shape(&offer, true)?;
    Ok(offer)
}

/// Verify an offer's durable structure and signature without applying wall-clock expiry.
///
/// Use this when replaying an offer already accepted into durable state. It does
/// not revive a removed or revoked offer; that policy belongs to the caller's
/// durable state machine.
pub fn verify_folder_share_offer(
    offer: &FolderShareOffer,
    trusted_source: &PublicIdentity,
    expected_target: DeviceId,
) -> Result<(), FolderSharingError> {
    validate_offer_shape(offer, true)?;
    verify_signature(
        trusted_source,
        OFFER_SIGNATURE_DOMAIN,
        &offer_signing_bytes(offer)?,
        &offer.signature,
    )?;
    if trusted_source.device_id != offer.source_device_id
        || expected_target != offer.target_device_id
    {
        return Err(FolderSharingError::WrongPeer);
    }
    Ok(())
}

/// Verify an offer during fresh intake, including its bounded acceptance window.
pub fn verify_fresh_folder_share_offer(
    offer: &FolderShareOffer,
    trusted_source: &PublicIdentity,
    expected_target: DeviceId,
    now_unix_ms: u64,
) -> Result<(), FolderSharingError> {
    verify_folder_share_offer(offer, trusted_source, expected_target)?;
    validate_fresh_offer_time(offer, now_unix_ms)
}

/// Return the domain-separated lowercase BLAKE3 digest of an entire signed offer.
pub fn folder_share_offer_digest(offer: &FolderShareOffer) -> Result<String, FolderSharingError> {
    validate_offer_shape(offer, true)?;
    digest_record(OFFER_DIGEST_DOMAIN, offer)
}

/// Sign a target acceptance after verifying a still-fresh source offer.
pub fn accept_folder_share_offer(
    offer: &FolderShareOffer,
    trusted_source: &PublicIdentity,
    target: &DeviceIdentity,
    target_engine: SyncEngineBinding,
    now_unix_ms: u64,
) -> Result<FolderShareAcceptance, FolderSharingError> {
    verify_fresh_folder_share_offer(offer, trusted_source, target.device_id(), now_unix_ms)?;
    validate_binding(&target_engine, target.device_id())?;
    let mut acceptance = FolderShareAcceptance {
        schema_version: offer.schema_version,
        offer_digest: folder_share_offer_digest(offer)?,
        target_device_id: target.device_id(),
        target_engine,
        accepted_at_unix_ms: now_unix_ms,
        signature: String::new(),
    };
    validate_acceptance_shape(offer, &acceptance, false)?;
    acceptance.signature = target.sign(
        ACCEPTANCE_SIGNATURE_DOMAIN,
        &acceptance_signing_bytes(&acceptance)?,
    );
    validate_acceptance_shape(offer, &acceptance, true)?;
    Ok(acceptance)
}

/// Verify an accepted share for durable replay without reapplying offer expiry.
pub fn verify_folder_share_acceptance(
    offer: &FolderShareOffer,
    acceptance: &FolderShareAcceptance,
    trusted_source: &PublicIdentity,
    trusted_target: &PublicIdentity,
) -> Result<(), FolderSharingError> {
    // Authenticate each record with the supplied trusted key before using its
    // claimed identity for routing or mismatch classification.
    verify_folder_share_offer(offer, trusted_source, offer.target_device_id)?;
    validate_acceptance_shape(offer, acceptance, true)?;
    verify_signature(
        trusted_target,
        ACCEPTANCE_SIGNATURE_DOMAIN,
        &acceptance_signing_bytes(acceptance)?,
        &acceptance.signature,
    )?;
    if acceptance.target_device_id != trusted_target.device_id
        || offer.target_device_id != trusted_target.device_id
    {
        return Err(FolderSharingError::WrongPeer);
    }
    Ok(())
}

/// Verify an acceptance during fresh intake, including the offer's live window.
pub fn verify_fresh_folder_share_acceptance(
    offer: &FolderShareOffer,
    acceptance: &FolderShareAcceptance,
    trusted_source: &PublicIdentity,
    trusted_target: &PublicIdentity,
    now_unix_ms: u64,
) -> Result<(), FolderSharingError> {
    verify_folder_share_acceptance(offer, acceptance, trusted_source, trusted_target)?;
    validate_fresh_offer_time(offer, now_unix_ms)?;
    if acceptance.accepted_at_unix_ms > now_unix_ms.saturating_add(MAX_CLOCK_SKEW_MS) {
        return Err(FolderSharingError::NotFresh);
    }
    Ok(())
}

/// Return the domain-separated lowercase BLAKE3 digest of an entire signed acceptance.
pub fn folder_share_acceptance_digest(
    offer: &FolderShareOffer,
    acceptance: &FolderShareAcceptance,
) -> Result<String, FolderSharingError> {
    validate_acceptance_shape(offer, acceptance, true)?;
    digest_record(ACCEPTANCE_DIGEST_DOMAIN, acceptance)
}

/// Create the source's final signed commitment to one fresh target acceptance.
pub fn commit_folder_share(
    offer: &FolderShareOffer,
    acceptance: &FolderShareAcceptance,
    source: &DeviceIdentity,
    trusted_target: &PublicIdentity,
    now_unix_ms: u64,
) -> Result<FolderShareCommit, FolderSharingError> {
    let trusted_source = source.public_identity();
    verify_fresh_folder_share_acceptance(
        offer,
        acceptance,
        &trusted_source,
        trusted_target,
        now_unix_ms,
    )?;
    let mut commit = FolderShareCommit {
        schema_version: offer.schema_version,
        source_device_id: source.device_id(),
        offer_digest: folder_share_offer_digest(offer)?,
        acceptance_digest: folder_share_acceptance_digest(offer, acceptance)?,
        signature: String::new(),
    };
    validate_commit_shape(offer, acceptance, &commit, false)?;
    commit.signature = source.sign(COMMIT_SIGNATURE_DOMAIN, &commit_signing_bytes(&commit)?);
    validate_commit_shape(offer, acceptance, &commit, true)?;
    Ok(commit)
}

/// Verify the complete three-record commitment for durable replay.
pub fn verify_folder_share_commit(
    offer: &FolderShareOffer,
    acceptance: &FolderShareAcceptance,
    commit: &FolderShareCommit,
    trusted_source: &PublicIdentity,
    trusted_target: &PublicIdentity,
) -> Result<(), FolderSharingError> {
    verify_folder_share_acceptance(offer, acceptance, trusted_source, trusted_target)?;
    validate_commit_shape(offer, acceptance, commit, true)?;
    verify_signature(
        trusted_source,
        COMMIT_SIGNATURE_DOMAIN,
        &commit_signing_bytes(commit)?,
        &commit.signature,
    )?;
    if commit.source_device_id != trusted_source.device_id {
        return Err(FolderSharingError::WrongPeer);
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OfferSigningFields<'a> {
    schema_version: u16,
    offer_id: Uuid,
    folder_id: Uuid,
    label: &'a str,
    source_device_id: DeviceId,
    target_device_id: DeviceId,
    source_engine: &'a SyncEngineBinding,
    #[serde(skip_serializing_if = "Option::is_none")]
    link_policy: Option<covalent_protocol::FolderLinkPolicy>,
    pairing_id: Option<&'a str>,
    issued_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    nonce: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceSigningFields<'a> {
    schema_version: u16,
    offer_digest: &'a str,
    target_device_id: DeviceId,
    target_engine: &'a SyncEngineBinding,
    accepted_at_unix_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CommitSigningFields<'a> {
    schema_version: u16,
    source_device_id: DeviceId,
    offer_digest: &'a str,
    acceptance_digest: &'a str,
}

fn offer_signing_bytes(offer: &FolderShareOffer) -> Result<Vec<u8>, FolderSharingError> {
    encode(&OfferSigningFields {
        schema_version: offer.schema_version,
        offer_id: offer.offer_id,
        folder_id: offer.folder_id,
        label: &offer.label,
        source_device_id: offer.source_device_id,
        target_device_id: offer.target_device_id,
        source_engine: &offer.source_engine,
        link_policy: offer.link_policy,
        pairing_id: offer.pairing_id.as_deref(),
        issued_at_unix_ms: offer.issued_at_unix_ms,
        expires_at_unix_ms: offer.expires_at_unix_ms,
        nonce: &offer.nonce,
    })
}

fn acceptance_signing_bytes(
    acceptance: &FolderShareAcceptance,
) -> Result<Vec<u8>, FolderSharingError> {
    encode(&AcceptanceSigningFields {
        schema_version: acceptance.schema_version,
        offer_digest: &acceptance.offer_digest,
        target_device_id: acceptance.target_device_id,
        target_engine: &acceptance.target_engine,
        accepted_at_unix_ms: acceptance.accepted_at_unix_ms,
    })
}

fn commit_signing_bytes(commit: &FolderShareCommit) -> Result<Vec<u8>, FolderSharingError> {
    encode(&CommitSigningFields {
        schema_version: commit.schema_version,
        source_device_id: commit.source_device_id,
        offer_digest: &commit.offer_digest,
        acceptance_digest: &commit.acceptance_digest,
    })
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, FolderSharingError> {
    serde_json::to_vec(value).map_err(|_| FolderSharingError::EncodingFailed)
}

fn digest_record(domain: &[u8], value: &impl Serialize) -> Result<String, FolderSharingError> {
    let encoded = encode(value)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&encoded);
    Ok(hasher.finalize().to_hex().to_string())
}

fn validate_offer_shape(
    offer: &FolderShareOffer,
    require_signature: bool,
) -> Result<(), FolderSharingError> {
    validate_label(&offer.label)?;
    validate_pairing_id(offer.pairing_id.as_deref())?;
    validate_binding(&offer.source_engine, offer.source_device_id)?;
    let expected_version = if offer.link_policy.is_some() {
        FOLDER_LINK_SCHEMA_VERSION
    } else {
        FOLDER_SHARE_SCHEMA_VERSION
    };
    if offer.schema_version != expected_version
        || offer.offer_id.is_nil()
        || offer.folder_id.is_nil()
        || is_nil_device_id(offer.source_device_id)
        || is_nil_device_id(offer.target_device_id)
        || offer.source_device_id == offer.target_device_id
        || offer.issued_at_unix_ms == 0
        || offer.expires_at_unix_ms <= offer.issued_at_unix_ms
        || offer.expires_at_unix_ms - offer.issued_at_unix_ms > MAX_FOLDER_SHARE_LIFETIME_MS
        || !is_canonical_base64(&offer.nonce, SHARE_NONCE_BYTES, true)
        || (require_signature
            && !is_canonical_base64(&offer.signature, ED25519_SIGNATURE_BYTES, false))
        || (!require_signature && !offer.signature.is_empty())
    {
        return Err(FolderSharingError::InvalidRecord);
    }
    Ok(())
}

fn validate_acceptance_shape(
    offer: &FolderShareOffer,
    acceptance: &FolderShareAcceptance,
    require_signature: bool,
) -> Result<(), FolderSharingError> {
    validate_binding(&acceptance.target_engine, acceptance.target_device_id)?;
    if acceptance.schema_version != offer.schema_version
        || acceptance.accepted_at_unix_ms == 0
        || acceptance.accepted_at_unix_ms >= offer.expires_at_unix_ms
        || acceptance
            .accepted_at_unix_ms
            .saturating_add(MAX_CLOCK_SKEW_MS)
            < offer.issued_at_unix_ms
        || !is_lower_hex_digest(&acceptance.offer_digest)
        || acceptance.offer_digest != folder_share_offer_digest(offer)?
        || (require_signature
            && !is_canonical_base64(&acceptance.signature, ED25519_SIGNATURE_BYTES, false))
        || (!require_signature && !acceptance.signature.is_empty())
    {
        return Err(FolderSharingError::InvalidRecord);
    }
    Ok(())
}

fn validate_commit_shape(
    offer: &FolderShareOffer,
    acceptance: &FolderShareAcceptance,
    commit: &FolderShareCommit,
    require_signature: bool,
) -> Result<(), FolderSharingError> {
    if commit.schema_version != offer.schema_version
        || !is_lower_hex_digest(&commit.offer_digest)
        || !is_lower_hex_digest(&commit.acceptance_digest)
        || commit.offer_digest != folder_share_offer_digest(offer)?
        || commit.acceptance_digest != folder_share_acceptance_digest(offer, acceptance)?
        || (require_signature
            && !is_canonical_base64(&commit.signature, ED25519_SIGNATURE_BYTES, false))
        || (!require_signature && !commit.signature.is_empty())
    {
        return Err(FolderSharingError::InvalidRecord);
    }
    Ok(())
}

fn validate_binding(
    binding: &SyncEngineBinding,
    expected_owner: DeviceId,
) -> Result<(), FolderSharingError> {
    binding
        .validate()
        .map_err(|_| FolderSharingError::InvalidEngineBinding)?;
    if binding.covalent_device_id != expected_owner {
        return Err(FolderSharingError::InvalidEngineBinding);
    }
    Ok(())
}

fn validate_label(value: &str) -> Result<(), FolderSharingError> {
    if value.trim().is_empty()
        || value.len() > MAX_FOLDER_SHARE_LABEL_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(FolderSharingError::InvalidRecord);
    }
    Ok(())
}

fn validate_pairing_id(value: Option<&str>) -> Result<(), FolderSharingError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.is_empty()
        || value.len() > MAX_FOLDER_SHARE_PAIRING_ID_BYTES
        || !is_canonical_base64(value, 16, true)
    {
        return Err(FolderSharingError::InvalidRecord);
    }
    Ok(())
}

fn validate_fresh_offer_time(
    offer: &FolderShareOffer,
    now_unix_ms: u64,
) -> Result<(), FolderSharingError> {
    if now_unix_ms == 0
        || now_unix_ms >= offer.expires_at_unix_ms
        || offer.issued_at_unix_ms > now_unix_ms.saturating_add(MAX_CLOCK_SKEW_MS)
    {
        return Err(FolderSharingError::NotFresh);
    }
    Ok(())
}

fn verify_signature(
    identity: &PublicIdentity,
    domain: &[u8],
    bytes: &[u8],
    signature: &str,
) -> Result<(), FolderSharingError> {
    identity
        .verify(domain, bytes, signature)
        .map_err(|_| FolderSharingError::InvalidSignature)
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == SHARE_DIGEST_BYTES * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_canonical_base64(value: &str, decoded_len: usize, nonzero: bool) -> bool {
    if value.len() > 128 {
        return false;
    }
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(value) else {
        return false;
    };
    decoded.len() == decoded_len
        && (!nonzero || decoded.iter().any(|byte| *byte != 0))
        && URL_SAFE_NO_PAD.encode(&decoded) == value
}

fn is_nil_device_id(value: DeviceId) -> bool {
    value == DeviceId::from_uuid(Uuid::nil())
}
