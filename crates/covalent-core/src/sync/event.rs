//! Closed plaintext envelopes for the future unified folder-event log.
//!
//! This module only routes bounded signed-record bytes to their authoritative
//! codec. Envelope validation is not signature checking, membership
//! authorization, causal admission, persistence, or a peer acknowledgement.

use std::fmt;

use thiserror::Error;
use zeroize::Zeroizing;

use super::bootstrap::MAX_SIGNED_BOOTSTRAP_RECORD_BYTES;
use super::freeze::MAX_SIGNED_FREEZE_RECORD_BYTES;
use super::ids::WriterId;
use super::log_frame::MAX_LOG_PLAINTEXT_BYTES;
use super::membership::MAX_SIGNED_EPOCH_BYTES;
use super::operation::MAX_SIGNED_OPERATION_BYTES;

const PRELUDE_BYTES: usize = 8 + 2 + 1;
const VERSION_OFFSET: usize = 8;
const FLAGS_OFFSET: usize = 10;
const WIRE_VERSION: u16 = 1;
const UUID_BYTES: usize = 16;
const DIGEST_BYTES: usize = 32;

// These offsets mirror the authoritative fixed-order encoders. Routing reads
// only the smallest prefix that contains the named fields.
const OPERATION_WRITER_OFFSET: usize = PRELUDE_BYTES + UUID_BYTES;
const RECEIPT_BINDING_DIGEST_OFFSET: usize = PRELUDE_BYTES;
const RECEIPT_SIGNER_OFFSET: usize = PRELUDE_BYTES + DIGEST_BYTES + UUID_BYTES + 8 + DIGEST_BYTES;
const ABORT_PROPOSAL_DIGEST_OFFSET: usize = PRELUDE_BYTES;

const OPERATION_MAGIC: &[u8; 8] = b"COVSOP01";
const MEMBERSHIP_EPOCH_MAGIC: &[u8; 8] = b"COVSEP01";
const BOOTSTRAP_PERMIT_MAGIC: &[u8; 8] = b"COVSBP01";
const BOOTSTRAP_RECEIPT_MAGIC: &[u8; 8] = b"COVSBR01";
const WRITE_LOSS_PROPOSAL_MAGIC: &[u8; 8] = b"COVSFP01";
const FREEZE_RECEIPT_MAGIC: &[u8; 8] = b"COVSFR01";
const FREEZE_ABORT_MAGIC: &[u8; 8] = b"COVSFA01";

const OPERATION_KNOWN_FLAGS: u8 = 1;
const MEMBERSHIP_KNOWN_FLAGS: u8 = 3;

/// Unauthenticated bytes that may route a bootstrap receipt to a permit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct UntrustedBootstrapPermitDigest([u8; DIGEST_BYTES]);

impl UntrustedBootstrapPermitDigest {
    /// Returns the untrusted fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

/// Unauthenticated bytes that may route freeze evidence to a proposal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct UntrustedWriteLossProposalDigest([u8; DIGEST_BYTES]);

impl UntrustedWriteLossProposalDigest {
    /// Returns the untrusted fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

/// Unauthenticated routing fields from a bootstrap-receipt prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UntrustedBootstrapReceiptHint {
    permit_digest: UntrustedBootstrapPermitDigest,
    candidate_writer_id: WriterId,
}

impl UntrustedBootstrapReceiptHint {
    /// Returns the untrusted exact-permit commitment candidate.
    #[must_use]
    pub const fn permit_digest(self) -> UntrustedBootstrapPermitDigest {
        self.permit_digest
    }

    /// Returns the untrusted candidate-writer routing identifier.
    #[must_use]
    pub const fn candidate_writer_id(self) -> WriterId {
        self.candidate_writer_id
    }
}

/// Unauthenticated routing fields from a freeze-receipt prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UntrustedFreezeReceiptHint {
    proposal_digest: UntrustedWriteLossProposalDigest,
    signer_writer_id: WriterId,
}

impl UntrustedFreezeReceiptHint {
    /// Returns the untrusted exact-proposal commitment candidate.
    #[must_use]
    pub const fn proposal_digest(self) -> UntrustedWriteLossProposalDigest {
        self.proposal_digest
    }

    /// Returns the untrusted receipt-signer routing identifier.
    #[must_use]
    pub const fn signer_writer_id(self) -> WriterId {
        self.signer_writer_id
    }
}

/// The complete version-1 set of plaintext folder-event kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EventKind {
    /// One canonical signed folder operation.
    Operation = 1,
    /// One canonical authority-signed membership epoch.
    MembershipEpoch = 2,
    /// One canonical authority-signed read-only bootstrap permit.
    BootstrapPermit = 3,
    /// One canonical candidate-signed bootstrap receipt.
    BootstrapReceipt = 4,
    /// One canonical authority-signed write-loss proposal.
    WriteLossProposal = 5,
    /// One canonical survivor-signed freeze receipt.
    FreezeReceipt = 6,
    /// One canonical authority-signed freeze abort.
    FreezeAbort = 7,
}

impl EventKind {
    fn from_tag(tag: u8) -> Result<Self, EventEnvelopeError> {
        match tag {
            1 => Ok(Self::Operation),
            2 => Ok(Self::MembershipEpoch),
            3 => Ok(Self::BootstrapPermit),
            4 => Ok(Self::BootstrapReceipt),
            5 => Ok(Self::WriteLossProposal),
            6 => Ok(Self::FreezeReceipt),
            7 => Ok(Self::FreezeAbort),
            _ => Err(EventEnvelopeError::UnknownKind),
        }
    }

    const fn maximum_record_bytes(self) -> usize {
        match self {
            Self::Operation => MAX_SIGNED_OPERATION_BYTES,
            Self::MembershipEpoch => MAX_SIGNED_EPOCH_BYTES,
            Self::BootstrapPermit | Self::BootstrapReceipt => MAX_SIGNED_BOOTSTRAP_RECORD_BYTES,
            Self::WriteLossProposal | Self::FreezeReceipt | Self::FreezeAbort => {
                MAX_SIGNED_FREEZE_RECORD_BYTES
            }
        }
    }

    const fn magic(self) -> &'static [u8; 8] {
        match self {
            Self::Operation => OPERATION_MAGIC,
            Self::MembershipEpoch => MEMBERSHIP_EPOCH_MAGIC,
            Self::BootstrapPermit => BOOTSTRAP_PERMIT_MAGIC,
            Self::BootstrapReceipt => BOOTSTRAP_RECEIPT_MAGIC,
            Self::WriteLossProposal => WRITE_LOSS_PROPOSAL_MAGIC,
            Self::FreezeReceipt => FREEZE_RECEIPT_MAGIC,
            Self::FreezeAbort => FREEZE_ABORT_MAGIC,
        }
    }

    const fn known_flags(self) -> u8 {
        match self {
            Self::Operation => OPERATION_KNOWN_FLAGS,
            Self::MembershipEpoch => MEMBERSHIP_KNOWN_FLAGS,
            Self::BootstrapPermit
            | Self::BootstrapReceipt
            | Self::WriteLossProposal
            | Self::FreezeReceipt
            | Self::FreezeAbort => 0,
        }
    }
}

/// A borrowed, structurally routed signed-record envelope.
///
/// The record has only passed common length and canonical-prelude checks. The
/// caller must use the matching codec to parse and verify the complete record,
/// then perform all transition and admission checks.
pub struct EventEnvelope<'a> {
    kind: EventKind,
    record: &'a [u8],
}

impl<'a> EventEnvelope<'a> {
    /// Parses `kind || record` without allocating or authenticating the record.
    pub fn parse(plaintext: &'a [u8]) -> Result<Self, EventEnvelopeError> {
        if plaintext.len() > MAX_LOG_PLAINTEXT_BYTES {
            return Err(EventEnvelopeError::RecordTooLarge);
        }
        let (&tag, record) = plaintext
            .split_first()
            .ok_or(EventEnvelopeError::EmptyEnvelope)?;
        Self::from_signed_record(EventKind::from_tag(tag)?, record)
    }

    /// Wraps already-signed record bytes after structural routing checks only.
    ///
    /// This does not verify that a signature is valid or that the signer is
    /// authorized. The authoritative kind-specific decoder remains required.
    pub fn from_signed_record(
        kind: EventKind,
        record: &'a [u8],
    ) -> Result<Self, EventEnvelopeError> {
        validate_record(kind, record)?;
        Ok(Self { kind, record })
    }

    /// Returns the untrusted routing kind.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        self.kind
    }

    /// Returns the unverified canonical-record candidate.
    #[must_use]
    pub const fn record(&self) -> &'a [u8] {
        self.record
    }

    /// Reads the untrusted writer routing hint from an operation prefix.
    ///
    /// The operation decoder must subsequently verify the complete record and
    /// reproduce this exact writer binding before the hint is used.
    pub fn untrusted_operation_writer_hint(&self) -> Result<WriterId, EventEnvelopeError> {
        self.require_kind(EventKind::Operation)?;
        read_writer_id(self.record, OPERATION_WRITER_OFFSET)
    }

    /// Reads untrusted permit and candidate routing hints from a bootstrap receipt.
    ///
    /// The authoritative bootstrap-receipt decoder must verify the complete
    /// record and reproduce both exact bindings before either hint is used.
    pub fn untrusted_bootstrap_receipt_hint(
        &self,
    ) -> Result<UntrustedBootstrapReceiptHint, EventEnvelopeError> {
        self.require_kind(EventKind::BootstrapReceipt)?;
        Ok(UntrustedBootstrapReceiptHint {
            permit_digest: UntrustedBootstrapPermitDigest(read_array(
                self.record,
                RECEIPT_BINDING_DIGEST_OFFSET,
            )?),
            candidate_writer_id: read_writer_id(self.record, RECEIPT_SIGNER_OFFSET)?,
        })
    }

    /// Reads untrusted proposal and signer routing hints from a freeze receipt.
    ///
    /// The authoritative freeze-receipt decoder must verify the complete
    /// record and reproduce both exact bindings before either hint is used.
    pub fn untrusted_freeze_receipt_hint(
        &self,
    ) -> Result<UntrustedFreezeReceiptHint, EventEnvelopeError> {
        self.require_kind(EventKind::FreezeReceipt)?;
        Ok(UntrustedFreezeReceiptHint {
            proposal_digest: UntrustedWriteLossProposalDigest(read_array(
                self.record,
                RECEIPT_BINDING_DIGEST_OFFSET,
            )?),
            signer_writer_id: read_writer_id(self.record, RECEIPT_SIGNER_OFFSET)?,
        })
    }

    /// Reads the untrusted proposal routing hint from a freeze abort.
    ///
    /// The authoritative abort decoder must verify the complete record and
    /// reproduce this exact proposal binding before the hint is used.
    pub fn untrusted_freeze_abort_proposal_hint(
        &self,
    ) -> Result<UntrustedWriteLossProposalDigest, EventEnvelopeError> {
        self.require_kind(EventKind::FreezeAbort)?;
        Ok(UntrustedWriteLossProposalDigest(read_array(
            self.record,
            ABORT_PROPOSAL_DIGEST_OFFSET,
        )?))
    }

    fn require_kind(&self, expected: EventKind) -> Result<(), EventEnvelopeError> {
        if self.kind != expected {
            return Err(EventEnvelopeError::WrongHintKind);
        }
        Ok(())
    }

    /// Encodes `kind || record` for immediate encrypted-frame construction.
    pub fn encode(&self) -> Result<EncodedEventEnvelope, EventEnvelopeError> {
        let encoded_length = self
            .record
            .len()
            .checked_add(1)
            .ok_or(EventEnvelopeError::RecordTooLarge)?;
        if encoded_length > MAX_LOG_PLAINTEXT_BYTES {
            return Err(EventEnvelopeError::RecordTooLarge);
        }
        let mut bytes = Zeroizing::new(Vec::new());
        bytes
            .try_reserve_exact(encoded_length)
            .map_err(|_| EventEnvelopeError::AllocationFailed)?;
        bytes.push(self.kind as u8);
        bytes.extend_from_slice(self.record);
        Ok(EncodedEventEnvelope(bytes))
    }
}

impl fmt::Debug for EventEnvelope<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventEnvelope")
            .field("kind", &self.kind)
            .field("record_length", &self.record.len())
            .finish_non_exhaustive()
    }
}

/// Owned plaintext bytes that zeroize on drop and redact record contents.
pub struct EncodedEventEnvelope(Zeroizing<Vec<u8>>);

impl EncodedEventEnvelope {
    /// Borrows the complete envelope for encrypted-frame construction.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl fmt::Debug for EncodedEventEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedEventEnvelope")
            .field("length", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// A fixed, non-sensitive envelope rejection.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EventEnvelopeError {
    /// The frame plaintext omitted its event-kind tag.
    #[error("folder event envelope is empty")]
    EmptyEnvelope,
    /// The event-kind tag is not part of the closed version-1 set.
    #[error("folder event kind is unknown")]
    UnknownKind,
    /// The complete envelope or kind-specific record exceeds its bound.
    #[error("folder event record exceeds its size limit")]
    RecordTooLarge,
    /// The record is too short to contain its canonical prelude.
    #[error("folder event record prelude is truncated")]
    TruncatedPrelude,
    /// The routing kind disagrees with the record codec magic.
    #[error("folder event record magic does not match its kind")]
    WrongRecordMagic,
    /// The record prelude names a version outside this envelope schema.
    #[error("folder event record version is unsupported")]
    UnsupportedRecordVersion,
    /// The record prelude sets flags unknown to its canonical codec.
    #[error("folder event record flags are unsupported")]
    UnsupportedRecordFlags,
    /// A routing accessor was called for another closed event kind.
    #[error("folder event routing hint does not match its event kind")]
    WrongHintKind,
    /// The record ends before the complete fixed routing prefix.
    #[error("folder event routing hint prefix is truncated")]
    TruncatedRoutingHint,
    /// A bounded encoded-result allocation failed.
    #[error("folder event envelope allocation failed")]
    AllocationFailed,
}

fn validate_record(kind: EventKind, record: &[u8]) -> Result<(), EventEnvelopeError> {
    let envelope_length = record
        .len()
        .checked_add(1)
        .ok_or(EventEnvelopeError::RecordTooLarge)?;
    if envelope_length > MAX_LOG_PLAINTEXT_BYTES || record.len() > kind.maximum_record_bytes() {
        return Err(EventEnvelopeError::RecordTooLarge);
    }
    if record.len() < PRELUDE_BYTES {
        return Err(EventEnvelopeError::TruncatedPrelude);
    }
    if record.get(..kind.magic().len()) != Some(kind.magic().as_slice()) {
        return Err(EventEnvelopeError::WrongRecordMagic);
    }
    let version = u16::from_be_bytes(
        record[VERSION_OFFSET..FLAGS_OFFSET]
            .try_into()
            .map_err(|_| EventEnvelopeError::TruncatedPrelude)?,
    );
    if version != WIRE_VERSION {
        return Err(EventEnvelopeError::UnsupportedRecordVersion);
    }
    if record[FLAGS_OFFSET] & !kind.known_flags() != 0 {
        return Err(EventEnvelopeError::UnsupportedRecordFlags);
    }
    Ok(())
}

fn read_writer_id(record: &[u8], offset: usize) -> Result<WriterId, EventEnvelopeError> {
    Ok(WriterId::from_uuid(uuid::Uuid::from_bytes(read_array(
        record, offset,
    )?)))
}

fn read_array<const N: usize>(record: &[u8], offset: usize) -> Result<[u8; N], EventEnvelopeError> {
    let end = offset
        .checked_add(N)
        .ok_or(EventEnvelopeError::TruncatedRoutingHint)?;
    record
        .get(offset..end)
        .ok_or(EventEnvelopeError::TruncatedRoutingHint)?
        .try_into()
        .map_err(|_| EventEnvelopeError::TruncatedRoutingHint)
}

#[cfg(test)]
mod tests {
    use covalent_protocol::DeviceId;
    use ed25519_dalek::SigningKey;
    use uuid::Uuid;

    use super::*;
    use crate::sync::bootstrap::{
        decode_signature_checked_bootstrap_permit, decode_signature_checked_bootstrap_receipt,
        encode_signed_bootstrap_permit, encode_signed_bootstrap_receipt,
    };
    use crate::sync::freeze::{
        WriteLossAction, WriteLossChange, decode_signature_checked_freeze_abort,
        decode_signature_checked_freeze_receipt, decode_signature_checked_write_loss_proposal,
        encode_signed_freeze_abort, encode_signed_freeze_receipt,
        encode_signed_write_loss_proposal,
    };
    use crate::sync::ids::{FolderId, WriterId};
    use crate::sync::membership::{EpochDigest, MemberGrant, MemberRole, WriterCutoff};
    use crate::sync::operation::{
        ClockEntry, decode_signature_checked_operation, encode_signed_operation,
    };

    const ALL_KINDS: [EventKind; 7] = [
        EventKind::Operation,
        EventKind::MembershipEpoch,
        EventKind::BootstrapPermit,
        EventKind::BootstrapReceipt,
        EventKind::WriteLossProposal,
        EventKind::FreezeReceipt,
        EventKind::FreezeAbort,
    ];

    fn structural_fixture_not_signature_evidence(kind: EventKind, length: usize) -> Vec<u8> {
        assert!(length >= PRELUDE_BYTES);
        let mut record = vec![0_u8; length];
        record[..8].copy_from_slice(kind.magic());
        record[VERSION_OFFSET..FLAGS_OFFSET].copy_from_slice(&WIRE_VERSION.to_be_bytes());
        record[FLAGS_OFFSET] = 0;
        record
    }

    fn valid_signed_operation() -> Vec<u8> {
        let signing_key = key(42);
        let folder = folder(1);
        let writer = writer(2);
        encode_signed_operation(
            &signing_key,
            folder,
            writer,
            1,
            [3; 32],
            1,
            None,
            &[ClockEntry::new(writer, 1).expect("clock")],
            b"opaque-body",
        )
        .expect("valid signed operation")
    }

    fn folder(number: u128) -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(number))
    }

    fn writer(number: u128) -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(number))
    }

    fn key(number: u8) -> SigningKey {
        let mut bytes = [0x27; 32];
        bytes[31] = number;
        SigningKey::from_bytes(&bytes)
    }

    fn member(number: u128, key_number: u8) -> MemberGrant {
        MemberGrant::new(
            writer(number),
            key(key_number).verifying_key(),
            DeviceId::from_uuid(Uuid::from_u128(1_000 + number)),
            key(key_number + 100).verifying_key(),
            MemberRole::ReadWrite,
        )
        .expect("member")
    }

    struct RoutingFixtures {
        operation: Vec<u8>,
        permit: Vec<u8>,
        bootstrap_receipt: Vec<u8>,
        proposal: Vec<u8>,
        freeze_receipt: Vec<u8>,
        freeze_abort: Vec<u8>,
    }

    fn routing_fixtures() -> RoutingFixtures {
        let operation = valid_signed_operation();
        let candidate = member(2, 2);
        let permit = encode_signed_bootstrap_permit(
            &key(1),
            folder(1),
            writer(1),
            4,
            EpochDigest::from_bytes([8; 32]),
            &candidate,
            [9; 32],
            1_000,
            &[],
        )
        .expect("permit");
        let checked_permit = decode_signature_checked_bootstrap_permit(
            &permit,
            folder(1),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("checked permit");
        let bootstrap_receipt =
            encode_signed_bootstrap_receipt(&key(2), &checked_permit, &[], [6; 32])
                .expect("bootstrap receipt");

        let proposal = encode_signed_write_loss_proposal(
            &key(1),
            folder(1),
            4,
            EpochDigest::from_bytes([8; 32]),
            writer(1),
            [7; 32],
            &[WriteLossChange::new(writer(2), WriteLossAction::Remove)],
            &[writer(1)],
            &[writer(2)],
        )
        .expect("proposal");
        let checked_proposal = decode_signature_checked_write_loss_proposal(
            &proposal,
            folder(1),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("checked proposal");
        let freeze_receipt = encode_signed_freeze_receipt(
            &key(1),
            &checked_proposal,
            &member(1, 1),
            &[],
            &[WriterCutoff::new(writer(2), 0, [0; 32]).expect("zero tip")],
        )
        .expect("freeze receipt");
        let freeze_abort =
            encode_signed_freeze_abort(&key(1), &checked_proposal).expect("freeze abort");
        RoutingFixtures {
            operation,
            permit,
            bootstrap_receipt,
            proposal,
            freeze_receipt,
            freeze_abort,
        }
    }

    #[test]
    fn all_seven_closed_kinds_round_trip_as_unverified_bytes() {
        for kind in ALL_KINDS {
            let record = structural_fixture_not_signature_evidence(kind, PRELUDE_BYTES + 9);
            let envelope = EventEnvelope::from_signed_record(kind, &record).expect("shape");
            assert_eq!(envelope.kind(), kind);
            assert_eq!(envelope.record(), record);
            let encoded = envelope.encode().expect("encode envelope");
            let reparsed = EventEnvelope::parse(encoded.as_bytes()).expect("parse envelope");
            assert_eq!(reparsed.kind(), kind);
            assert_eq!(reparsed.record(), record);
        }
    }

    #[test]
    fn unknown_empty_and_wrong_kind_envelopes_fail_closed() {
        assert!(matches!(
            EventEnvelope::parse(&[]),
            Err(EventEnvelopeError::EmptyEnvelope)
        ));
        assert!(matches!(
            EventEnvelope::parse(&[0]),
            Err(EventEnvelopeError::UnknownKind)
        ));

        let operation = valid_signed_operation();
        assert!(matches!(
            EventEnvelope::from_signed_record(EventKind::MembershipEpoch, &operation),
            Err(EventEnvelopeError::WrongRecordMagic)
        ));
        let mut wrongly_tagged = vec![EventKind::MembershipEpoch as u8];
        wrongly_tagged.extend_from_slice(&operation);
        assert!(matches!(
            EventEnvelope::parse(&wrongly_tagged),
            Err(EventEnvelopeError::WrongRecordMagic)
        ));
    }

    #[test]
    fn every_kind_enforces_its_exact_record_cap_before_encoding() {
        for kind in ALL_KINDS {
            let maximum = kind.maximum_record_bytes();
            let record = structural_fixture_not_signature_evidence(kind, maximum);
            let envelope = EventEnvelope::from_signed_record(kind, &record).expect("exact cap");
            assert_eq!(
                envelope.encode().expect("encode cap").as_bytes().len(),
                maximum + 1
            );

            let oversized = structural_fixture_not_signature_evidence(kind, maximum + 1);
            assert!(matches!(
                EventEnvelope::from_signed_record(kind, &oversized),
                Err(EventEnvelopeError::RecordTooLarge)
            ));
        }
    }

    #[test]
    fn common_envelope_cap_rejects_before_record_inspection_or_copy() {
        let oversized = vec![0xff; MAX_LOG_PLAINTEXT_BYTES + 1];
        assert!(matches!(
            EventEnvelope::parse(&oversized),
            Err(EventEnvelopeError::RecordTooLarge)
        ));
    }

    #[test]
    fn every_truncated_canonical_prelude_is_rejected() {
        for kind in ALL_KINDS {
            let complete = structural_fixture_not_signature_evidence(kind, PRELUDE_BYTES);
            for length in 0..PRELUDE_BYTES {
                let mut plaintext = vec![kind as u8];
                plaintext.extend_from_slice(&complete[..length]);
                assert!(matches!(
                    EventEnvelope::parse(&plaintext),
                    Err(EventEnvelopeError::TruncatedPrelude)
                ));
            }
            let mut plaintext = vec![kind as u8];
            plaintext.extend_from_slice(&complete);
            EventEnvelope::parse(&plaintext).expect("complete prelude routes");
        }
    }

    #[test]
    fn version_and_kind_specific_flags_are_checked_without_parsing_records() {
        for kind in ALL_KINDS {
            let mut record = structural_fixture_not_signature_evidence(kind, PRELUDE_BYTES);
            record[VERSION_OFFSET..FLAGS_OFFSET].copy_from_slice(&2_u16.to_be_bytes());
            assert!(matches!(
                EventEnvelope::from_signed_record(kind, &record),
                Err(EventEnvelopeError::UnsupportedRecordVersion)
            ));

            record[VERSION_OFFSET..FLAGS_OFFSET].copy_from_slice(&WIRE_VERSION.to_be_bytes());
            record[FLAGS_OFFSET] = match kind {
                EventKind::Operation => 2,
                EventKind::MembershipEpoch => 4,
                _ => 1,
            };
            assert!(matches!(
                EventEnvelope::from_signed_record(kind, &record),
                Err(EventEnvelopeError::UnsupportedRecordFlags)
            ));
        }
    }

    #[test]
    fn debug_output_never_contains_record_bytes() {
        let marker = b"private-record-marker";
        let mut record = structural_fixture_not_signature_evidence(
            EventKind::Operation,
            PRELUDE_BYTES + marker.len(),
        );
        record[PRELUDE_BYTES..].copy_from_slice(marker);
        let envelope =
            EventEnvelope::from_signed_record(EventKind::Operation, &record).expect("shape");
        let encoded = envelope.encode().expect("encode");
        assert!(!format!("{envelope:?}").contains("private-record-marker"));
        assert!(!format!("{encoded:?}").contains("private-record-marker"));
    }

    #[test]
    fn untrusted_hints_match_authoritative_decoders_on_real_signed_records() {
        let fixtures = routing_fixtures();

        let operation =
            EventEnvelope::from_signed_record(EventKind::Operation, &fixtures.operation)
                .expect("operation envelope");
        let operation_hint = operation
            .untrusted_operation_writer_hint()
            .expect("operation hint");
        let checked_operation = decode_signature_checked_operation(
            &fixtures.operation,
            folder(1),
            writer(2),
            &key(42).verifying_key(),
        )
        .expect("checked operation");
        assert_eq!(operation_hint, checked_operation.header().writer_id());

        let checked_permit = decode_signature_checked_bootstrap_permit(
            &fixtures.permit,
            folder(1),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("checked permit");
        let bootstrap = EventEnvelope::from_signed_record(
            EventKind::BootstrapReceipt,
            &fixtures.bootstrap_receipt,
        )
        .expect("bootstrap envelope");
        let bootstrap_hint = bootstrap
            .untrusted_bootstrap_receipt_hint()
            .expect("bootstrap hint");
        let checked_bootstrap = decode_signature_checked_bootstrap_receipt(
            &fixtures.bootstrap_receipt,
            &checked_permit,
            &key(2).verifying_key(),
        )
        .expect("checked bootstrap receipt");
        assert_eq!(
            bootstrap_hint.permit_digest().to_bytes(),
            checked_bootstrap.permit_digest().to_bytes()
        );
        assert_eq!(
            bootstrap_hint.candidate_writer_id(),
            checked_bootstrap.candidate_writer_id()
        );

        let checked_proposal = decode_signature_checked_write_loss_proposal(
            &fixtures.proposal,
            folder(1),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("checked proposal");
        let freeze =
            EventEnvelope::from_signed_record(EventKind::FreezeReceipt, &fixtures.freeze_receipt)
                .expect("freeze envelope");
        let freeze_hint = freeze.untrusted_freeze_receipt_hint().expect("freeze hint");
        let checked_freeze = decode_signature_checked_freeze_receipt(
            &fixtures.freeze_receipt,
            &checked_proposal,
            &member(1, 1),
        )
        .expect("checked freeze receipt");
        assert_eq!(
            freeze_hint.proposal_digest().to_bytes(),
            checked_freeze.proposal_digest().to_bytes()
        );
        assert_eq!(
            freeze_hint.signer_writer_id(),
            checked_freeze.signer_writer_id()
        );

        let abort =
            EventEnvelope::from_signed_record(EventKind::FreezeAbort, &fixtures.freeze_abort)
                .expect("abort envelope");
        let abort_hint = abort
            .untrusted_freeze_abort_proposal_hint()
            .expect("abort hint");
        let checked_abort = decode_signature_checked_freeze_abort(
            &fixtures.freeze_abort,
            &checked_proposal,
            &key(1).verifying_key(),
        )
        .expect("checked abort");
        assert_eq!(
            abort_hint.to_bytes(),
            checked_abort.proposal_digest().to_bytes()
        );
    }

    #[test]
    fn routing_hints_reject_wrong_event_kinds() {
        let operation = valid_signed_operation();
        let envelope =
            EventEnvelope::from_signed_record(EventKind::Operation, &operation).expect("envelope");
        assert_eq!(
            envelope.untrusted_bootstrap_receipt_hint(),
            Err(EventEnvelopeError::WrongHintKind)
        );
        assert_eq!(
            envelope.untrusted_freeze_receipt_hint(),
            Err(EventEnvelopeError::WrongHintKind)
        );
        assert_eq!(
            envelope.untrusted_freeze_abort_proposal_hint(),
            Err(EventEnvelopeError::WrongHintKind)
        );

        let receipt = structural_fixture_not_signature_evidence(
            EventKind::BootstrapReceipt,
            RECEIPT_SIGNER_OFFSET + UUID_BYTES,
        );
        let envelope = EventEnvelope::from_signed_record(EventKind::BootstrapReceipt, &receipt)
            .expect("receipt envelope");
        assert_eq!(
            envelope.untrusted_operation_writer_hint(),
            Err(EventEnvelopeError::WrongHintKind)
        );
    }

    #[test]
    fn every_routing_prefix_truncation_is_rejected() {
        let fixtures = routing_fixtures();
        for length in 0..OPERATION_WRITER_OFFSET + UUID_BYTES {
            let envelope = EventEnvelope {
                kind: EventKind::Operation,
                record: &fixtures.operation[..length],
            };
            assert_eq!(
                envelope.untrusted_operation_writer_hint(),
                Err(EventEnvelopeError::TruncatedRoutingHint),
                "operation prefix {length}"
            );
        }
        for length in 0..RECEIPT_SIGNER_OFFSET + UUID_BYTES {
            let bootstrap = EventEnvelope {
                kind: EventKind::BootstrapReceipt,
                record: &fixtures.bootstrap_receipt[..length],
            };
            assert_eq!(
                bootstrap.untrusted_bootstrap_receipt_hint(),
                Err(EventEnvelopeError::TruncatedRoutingHint),
                "bootstrap prefix {length}"
            );
            let freeze = EventEnvelope {
                kind: EventKind::FreezeReceipt,
                record: &fixtures.freeze_receipt[..length],
            };
            assert_eq!(
                freeze.untrusted_freeze_receipt_hint(),
                Err(EventEnvelopeError::TruncatedRoutingHint),
                "freeze prefix {length}"
            );
        }
        for length in 0..ABORT_PROPOSAL_DIGEST_OFFSET + DIGEST_BYTES {
            let envelope = EventEnvelope {
                kind: EventKind::FreezeAbort,
                record: &fixtures.freeze_abort[..length],
            };
            assert_eq!(
                envelope.untrusted_freeze_abort_proposal_hint(),
                Err(EventEnvelopeError::TruncatedRoutingHint),
                "abort prefix {length}"
            );
        }
    }

    #[test]
    fn hints_from_malformed_bodies_never_become_checked_records() {
        let fixtures = routing_fixtures();

        let operation_record = &fixtures.operation[..OPERATION_WRITER_OFFSET + UUID_BYTES];
        let operation = EventEnvelope::from_signed_record(EventKind::Operation, operation_record)
            .expect("routable operation prefix");
        assert_eq!(operation.untrusted_operation_writer_hint(), Ok(writer(2)));
        assert!(
            decode_signature_checked_operation(
                operation_record,
                folder(1),
                writer(2),
                &key(42).verifying_key(),
            )
            .is_err()
        );

        let checked_permit = decode_signature_checked_bootstrap_permit(
            &fixtures.permit,
            folder(1),
            writer(1),
            &key(1).verifying_key(),
        )
        .unwrap();
        let bootstrap_record = &fixtures.bootstrap_receipt[..RECEIPT_SIGNER_OFFSET + UUID_BYTES];
        let bootstrap =
            EventEnvelope::from_signed_record(EventKind::BootstrapReceipt, bootstrap_record)
                .expect("routable bootstrap prefix");
        assert!(bootstrap.untrusted_bootstrap_receipt_hint().is_ok());
        assert!(
            decode_signature_checked_bootstrap_receipt(
                bootstrap_record,
                &checked_permit,
                &key(2).verifying_key(),
            )
            .is_err()
        );

        let checked_proposal = decode_signature_checked_write_loss_proposal(
            &fixtures.proposal,
            folder(1),
            writer(1),
            &key(1).verifying_key(),
        )
        .unwrap();
        let freeze_record = &fixtures.freeze_receipt[..RECEIPT_SIGNER_OFFSET + UUID_BYTES];
        let freeze = EventEnvelope::from_signed_record(EventKind::FreezeReceipt, freeze_record)
            .expect("routable freeze prefix");
        assert!(freeze.untrusted_freeze_receipt_hint().is_ok());
        assert!(
            decode_signature_checked_freeze_receipt(
                freeze_record,
                &checked_proposal,
                &member(1, 1),
            )
            .is_err()
        );

        let abort_record = &fixtures.freeze_abort[..ABORT_PROPOSAL_DIGEST_OFFSET + DIGEST_BYTES];
        let abort = EventEnvelope::from_signed_record(EventKind::FreezeAbort, abort_record)
            .expect("routable abort prefix");
        assert!(abort.untrusted_freeze_abort_proposal_hint().is_ok());
        assert!(
            decode_signature_checked_freeze_abort(
                abort_record,
                &checked_proposal,
                &key(1).verifying_key(),
            )
            .is_err()
        );
    }
}
