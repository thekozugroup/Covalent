use std::fs;
use std::os::unix::fs::PermissionsExt;

use covalent_protocol::DeviceId;
use ed25519_dalek::SigningKey;
use uuid::Uuid;

use super::*;
use crate::sync::bootstrap::{
    decode_signature_checked_bootstrap_permit, encode_signed_bootstrap_permit,
    encode_signed_bootstrap_receipt,
};
use crate::sync::event_log::{DurableEventLog, EventLogLimits};
use crate::sync::freeze::{
    SignatureCheckedWriteLossProposal, WriteLossAction, WriteLossChange,
    decode_signature_checked_freeze_receipt, decode_signature_checked_write_loss_proposal,
    encode_signed_freeze_abort, encode_signed_write_loss_proposal,
};
use crate::sync::ids::FolderId;
use crate::sync::log_frame::{LogBinding, LogFrameKey};
use crate::sync::machine::{
    FolderFreezeState, FolderMachineConfig, FolderMachineLimits, FreezeReceiptState,
};
use crate::sync::membership::{
    EpochDigest, MemberGrant, MemberRole, decode_signature_checked_epoch, encode_signed_epoch,
};
use crate::sync::operation::{
    ClockEntry, decode_signature_checked_operation, encode_signed_operation,
};
use crate::sync::path::SyncPath;
use crate::sync::state_dir::{PrivateStateDir, StateKey};

const FOLDER_NUMBER: u128 = 401;
const AUTHORITY_WRITER_NUMBER: u128 = 402;
const CANDIDATE_WRITER_NUMBER: u128 = 403;
const READER_WRITER_NUMBER: u128 = 404;

fn folder() -> FolderId {
    FolderId::from_uuid(Uuid::from_u128(FOLDER_NUMBER))
}

fn writer(number: u128) -> WriterId {
    WriterId::from_uuid(Uuid::from_u128(number))
}

fn device(number: u128) -> DeviceId {
    DeviceId::from_uuid(Uuid::from_u128(number))
}

fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn authority_key() -> SigningKey {
    key(41)
}

fn authority_writer() -> WriterId {
    writer(AUTHORITY_WRITER_NUMBER)
}

fn candidate_writer() -> WriterId {
    writer(CANDIDATE_WRITER_NUMBER)
}

fn reader_writer() -> WriterId {
    writer(READER_WRITER_NUMBER)
}

fn grant(
    writer_id: WriterId,
    signing_key: &SigningKey,
    transport_number: u128,
    role: MemberRole,
) -> MemberGrant {
    MemberGrant::new(
        writer_id,
        signing_key.verifying_key(),
        device(transport_number),
        key(transport_number as u8).verifying_key(),
        role,
    )
    .expect("grant")
}

fn authority_grant() -> MemberGrant {
    grant(
        authority_writer(),
        &authority_key(),
        411,
        MemberRole::ReadWrite,
    )
}

fn binding() -> LogBinding {
    LogBinding::new(
        folder(),
        Uuid::from_u128(412),
        Uuid::from_u128(413),
        LogFileKind::FolderEvents,
    )
}

fn frame_key() -> LogFrameKey {
    LogFrameKey::from_bytes([42; 32])
}

fn log_limits(maximum_records: u64) -> EventLogLimits {
    EventLogLimits {
        maximum_bytes: 4 * 1_024 * 1_024,
        maximum_records,
    }
}

fn machine(local_writer: WriterId) -> FolderEventMachine {
    FolderEventMachine::new(FolderMachineConfig {
        folder_id: folder(),
        authority_writer_id: authority_writer(),
        pinned_authority_key: authority_key().verifying_key(),
        local_writer_id: local_writer,
        limits: FolderMachineLimits {
            maximum_index_bytes: 8 * 1_024 * 1_024,
            maximum_operations: 128,
            maximum_paths: 128,
            maximum_membership_epochs: 128,
            maximum_pending_evidence_bytes: 1 << 20,
            maximum_pending_evidence_records: 128,
        },
    })
    .expect("machine")
}

fn event(kind: EventKind, record: &[u8]) -> Vec<u8> {
    EventEnvelope::from_signed_record(kind, record)
        .expect("event")
        .encode()
        .expect("encoded event")
        .as_bytes()
        .to_vec()
}

fn genesis() -> (Vec<u8>, EpochDigest) {
    let record = encode_signed_epoch(
        &authority_key(),
        folder(),
        1,
        authority_writer(),
        None,
        &[authority_grant()],
        None,
        &[],
        &[],
        &[],
        &[],
    )
    .expect("genesis");
    let digest = decode_signature_checked_epoch(
        &record,
        folder(),
        authority_writer(),
        &authority_key().verifying_key(),
    )
    .expect("checked genesis")
    .digest();
    (event(EventKind::MembershipEpoch, &record), digest)
}

fn bootstrap(
    base_epoch: u64,
    base_digest: EpochDigest,
    candidate: &MemberGrant,
    candidate_key: &SigningKey,
    nonce: u8,
) -> (Vec<u8>, Vec<u8>, [u8; 32]) {
    let permit_record = encode_signed_bootstrap_permit(
        &authority_key(),
        folder(),
        authority_writer(),
        base_epoch,
        base_digest,
        candidate,
        [nonce; 32],
        1,
        &[],
    )
    .expect("permit");
    let permit = decode_signature_checked_bootstrap_permit(
        &permit_record,
        folder(),
        authority_writer(),
        &authority_key().verifying_key(),
    )
    .expect("checked permit");
    let receipt_record =
        encode_signed_bootstrap_receipt(candidate_key, &permit, &[], [nonce + 1; 32])
            .expect("receipt");
    let receipt = crate::sync::bootstrap::decode_signature_checked_bootstrap_receipt(
        &receipt_record,
        &permit,
        candidate.writer_key(),
    )
    .expect("checked receipt");
    (
        event(EventKind::BootstrapPermit, &permit_record),
        event(EventKind::BootstrapReceipt, &receipt_record),
        receipt.digest().to_bytes(),
    )
}

fn epoch(
    number: u64,
    previous: EpochDigest,
    roster: &[MemberGrant],
    bootstrap_receipts: &[[u8; 32]],
) -> (Vec<u8>, EpochDigest) {
    let record = encode_signed_epoch(
        &authority_key(),
        folder(),
        number,
        authority_writer(),
        Some(previous),
        roster,
        None,
        &[],
        &[],
        &[],
        bootstrap_receipts,
    )
    .expect("epoch");
    let digest = decode_signature_checked_epoch(
        &record,
        folder(),
        authority_writer(),
        &authority_key().verifying_key(),
    )
    .expect("checked epoch")
    .digest();
    (event(EventKind::MembershipEpoch, &record), digest)
}

fn proposal(
    base_epoch: u64,
    base_digest: EpochDigest,
    changes: &[WriteLossChange],
    survivors: &[WriterId],
    losing: &[WriterId],
) -> (Vec<u8>, SignatureCheckedWriteLossProposal) {
    let record = encode_signed_write_loss_proposal(
        &authority_key(),
        folder(),
        base_epoch,
        base_digest,
        authority_writer(),
        [43; 32],
        changes,
        survivors,
        losing,
    )
    .expect("proposal");
    let checked = decode_signature_checked_write_loss_proposal(
        &record,
        folder(),
        authority_writer(),
        &authority_key().verifying_key(),
    )
    .expect("checked proposal");
    (event(EventKind::WriteLossProposal, &record), checked)
}

struct FreezeSetup {
    temp: tempfile::TempDir,
    log: DurableFolderLog,
    proposal: SignatureCheckedWriteLossProposal,
    candidate_operation_digest: [u8; 32],
}

fn authority_survivor_setup(maximum_records: u64) -> FreezeSetup {
    let temp = tempfile::tempdir().expect("temp");
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).expect("mode");
    let directory = PrivateStateDir::open_root(temp.path()).expect("state");
    let log = DurableEventLog::create(
        &directory,
        &StateKey::new("events.v1").expect("name"),
        binding(),
        frame_key(),
        log_limits(maximum_records),
        machine(authority_writer()),
    )
    .expect("log");
    let mut log = DurableFolderLog::new(log, authority_writer(), authority_key()).expect("wrapper");
    let (genesis, genesis_digest) = genesis();
    log.ingest(&genesis).expect("genesis append");
    let candidate_key = key(44);
    let candidate_grant = grant(
        candidate_writer(),
        &candidate_key,
        414,
        MemberRole::ReadWrite,
    );
    let (permit, receipt, receipt_digest) =
        bootstrap(1, genesis_digest, &candidate_grant, &candidate_key, 45);
    let (epoch_two, epoch_two_digest) = epoch(
        2,
        genesis_digest,
        &[authority_grant(), candidate_grant.clone()],
        &[receipt_digest],
    );
    for entry in [&permit, &receipt, &epoch_two] {
        log.ingest(entry).expect("membership append");
    }
    let candidate_record = encode_signed_operation(
        &candidate_key,
        folder(),
        candidate_writer(),
        2,
        epoch_two_digest.to_bytes(),
        1,
        None,
        &[ClockEntry::new(candidate_writer(), 1).expect("clock")],
        &OperationBody::new(
            SyncPath::from_wire("candidate-history").expect("path"),
            EntryValue::Directory,
        )
        .encode(),
    )
    .expect("candidate operation");
    let candidate_checked = decode_signature_checked_operation(
        &candidate_record,
        folder(),
        candidate_writer(),
        &candidate_key.verifying_key(),
    )
    .expect("checked candidate operation");
    log.ingest(&event(EventKind::Operation, &candidate_record))
        .expect("candidate operation append");
    log.publish_local(&OperationBody::new(
        SyncPath::from_wire("authority-history").expect("path"),
        EntryValue::Directory,
    ))
    .expect("authority operation");
    let (proposal_event, proposal) = proposal(
        2,
        epoch_two_digest,
        &[WriteLossChange::new(
            candidate_writer(),
            WriteLossAction::Remove,
        )],
        &[authority_writer()],
        &[candidate_writer()],
    );
    log.ingest(&proposal_event).expect("proposal append");
    FreezeSetup {
        temp,
        log,
        proposal,
        candidate_operation_digest: candidate_checked.digest().to_bytes(),
    }
}

fn reopen(
    temp: &tempfile::TempDir,
    maximum_records: u64,
    local_writer: WriterId,
    wrapper_writer: WriterId,
    wrapper_key: SigningKey,
) -> DurableFolderLog {
    let directory = PrivateStateDir::open_root(temp.path()).expect("state");
    let log = DurableEventLog::open(
        &directory,
        &StateKey::new("events.v1").expect("name"),
        binding(),
        frame_key(),
        log_limits(maximum_records),
        machine(local_writer),
    )
    .expect("reopen log");
    DurableFolderLog::new(log, wrapper_writer, wrapper_key).expect("wrapper")
}

#[test]
fn local_receipt_derives_full_history_and_retries_immutable_after_growth_and_reopen() {
    let FreezeSetup {
        temp,
        mut log,
        proposal,
        candidate_operation_digest,
        ..
    } = authority_survivor_setup(64);
    let digest = proposal.digest().to_bytes();
    let first = log
        .publish_local_freeze_receipt(digest)
        .expect("publish receipt");
    assert!(first.ordinal().is_some());
    let first_bytes = first.event_bytes().to_vec();
    let envelope = EventEnvelope::parse(&first_bytes).expect("receipt envelope");
    let checked =
        decode_signature_checked_freeze_receipt(envelope.record(), &proposal, &authority_grant())
            .expect("checked receipt");
    assert_eq!(checked.frontier_entries().len(), 2);
    assert_eq!(
        checked
            .frontier()
            .counter(authority_writer().into_vector_actor()),
        1
    );
    assert_eq!(
        checked
            .frontier()
            .counter(candidate_writer().into_vector_actor()),
        1
    );
    assert_eq!(checked.losing_writer_tips().len(), 1);
    assert_eq!(
        checked.losing_writer_tips()[0].writer_id(),
        candidate_writer()
    );
    assert_eq!(checked.losing_writer_tips()[0].counter(), 1);
    assert_eq!(
        checked.losing_writer_tips()[0].operation_digest(),
        candidate_operation_digest
    );
    let FolderFreezeState::Pending(pending) = log.machine().expect("machine").freeze_state() else {
        panic!("pending freeze");
    };
    assert_eq!(
        pending.receipt_state(authority_writer()),
        Some(FreezeReceiptState::Resolved)
    );

    log.publish_local(&OperationBody::new(
        SyncPath::from_wire("later-survivor-history").expect("path"),
        EntryValue::Directory,
    ))
    .expect("later operation");
    let retry = log
        .publish_local_freeze_receipt(digest)
        .expect("immutable retry");
    assert_eq!(retry.ordinal(), None);
    assert_eq!(retry.event_bytes(), first_bytes);
    drop(log);

    let mut reopened = reopen(
        &temp,
        64,
        authority_writer(),
        authority_writer(),
        authority_key(),
    );
    let replayed = reopened
        .publish_local_freeze_receipt(digest)
        .expect("replayed retry");
    assert_eq!(replayed.ordinal(), None);
    assert_eq!(replayed.event_bytes(), first_bytes);
    assert!(!format!("{replayed:?}").contains("candidate-history"));
}

#[test]
fn downgraded_and_read_only_survivors_can_sign_but_removed_local_writer_cannot() {
    // A current read-write member named by DowngradeToRead remains a survivor.
    let mut downgraded = authority_survivor_setup(64);
    let base = downgraded
        .log
        .machine()
        .expect("machine")
        .current_head()
        .expect("head");
    let (downgrade_event, downgrade_proposal) = proposal(
        base.epoch(),
        base.digest(),
        &[WriteLossChange::new(
            candidate_writer(),
            WriteLossAction::DowngradeToRead,
        )],
        &[authority_writer(), candidate_writer()],
        &[candidate_writer()],
    );
    // The existing removal proposal is still pending, so construct a separate local-candidate log.
    let temp = tempfile::tempdir().expect("temp");
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).expect("mode");
    let directory = PrivateStateDir::open_root(temp.path()).expect("state");
    let raw = DurableEventLog::create(
        &directory,
        &StateKey::new("events.v1").expect("name"),
        binding(),
        frame_key(),
        log_limits(64),
        machine(candidate_writer()),
    )
    .expect("log");
    let mut local_candidate =
        DurableFolderLog::new(raw, candidate_writer(), key(44)).expect("wrapper");
    let page_limits = EventLogPageLimits {
        maximum_records: 64,
        maximum_plaintext_bytes: 1 << 20,
    };
    let cursor = downgraded.log.start_cursor().expect("cursor");
    let page = downgraded
        .log
        .read_page(&cursor, page_limits)
        .expect("page");
    for entry in page.events().iter().take(6) {
        local_candidate
            .ingest(entry.event_bytes())
            .expect("history append");
    }
    local_candidate
        .ingest(&downgrade_event)
        .expect("downgrade proposal");
    assert!(
        local_candidate
            .publish_local_freeze_receipt(downgrade_proposal.digest().to_bytes())
            .is_ok()
    );

    // The removal proposal from the first log excludes the local candidate.
    drop(downgraded.log);
    let mut removed = reopen(
        &downgraded.temp,
        64,
        candidate_writer(),
        candidate_writer(),
        key(44),
    );
    assert!(matches!(
        removed.publish_local_freeze_receipt(downgraded.proposal.digest().to_bytes()),
        Err(PublicationError::FreezeReceiptDenied)
    ));

    // A read-only local member can sign for another losing writer.
    let reader_key = key(46);
    let reader = grant(reader_writer(), &reader_key, 415, MemberRole::Read);
    let loser_key = key(47);
    let loser = grant(candidate_writer(), &loser_key, 416, MemberRole::ReadWrite);
    let temp = tempfile::tempdir().expect("temp");
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).expect("mode");
    let directory = PrivateStateDir::open_root(temp.path()).expect("state");
    let raw = DurableEventLog::create(
        &directory,
        &StateKey::new("events.v1").expect("name"),
        binding(),
        frame_key(),
        log_limits(64),
        machine(reader_writer()),
    )
    .expect("log");
    let mut read_log = DurableFolderLog::new(raw, reader_writer(), reader_key).expect("wrapper");
    let (genesis, genesis_digest) = genesis();
    read_log.ingest(&genesis).expect("genesis");
    let (reader_permit, reader_receipt, reader_receipt_digest) =
        bootstrap(1, genesis_digest, &reader, &key(46), 48);
    let (epoch_two, epoch_two_digest) = epoch(
        2,
        genesis_digest,
        &[authority_grant(), reader.clone()],
        &[reader_receipt_digest],
    );
    for entry in [&reader_permit, &reader_receipt, &epoch_two] {
        read_log.ingest(entry).expect("reader membership");
    }
    let (loser_permit, loser_receipt, loser_receipt_digest) =
        bootstrap(2, epoch_two_digest, &loser, &loser_key, 49);
    let (epoch_three, epoch_three_digest) = epoch(
        3,
        epoch_two_digest,
        &[authority_grant(), loser, reader],
        &[loser_receipt_digest],
    );
    for entry in [&loser_permit, &loser_receipt, &epoch_three] {
        read_log.ingest(entry).expect("loser membership");
    }
    let (proposal_event, checked) = proposal(
        3,
        epoch_three_digest,
        &[WriteLossChange::new(
            candidate_writer(),
            WriteLossAction::Remove,
        )],
        &[authority_writer(), reader_writer()],
        &[candidate_writer()],
    );
    read_log.ingest(&proposal_event).expect("proposal");
    assert!(
        read_log
            .publish_local_freeze_receipt(checked.digest().to_bytes())
            .is_ok()
    );
}

#[test]
fn wrong_identity_key_digest_and_full_log_return_no_receipt_or_machine_mutation() {
    let setup = authority_survivor_setup(7);
    let digest = setup.proposal.digest().to_bytes();
    let revision = setup
        .log
        .machine()
        .expect("machine")
        .current_frontier()
        .clone();
    let mut log = setup.log;
    assert!(matches!(
        log.publish_local_freeze_receipt([0; 32]),
        Err(PublicationError::FreezeReceiptDenied)
    ));
    assert_eq!(
        log.machine().expect("machine").current_frontier(),
        &revision
    );
    assert!(matches!(
        log.publish_local_freeze_receipt(digest),
        Err(PublicationError::Log(EventLogError::QuotaExceeded))
    ));
    let FolderFreezeState::Pending(pending) = log.machine().expect("machine").freeze_state() else {
        panic!("pending freeze");
    };
    assert_eq!(
        pending.receipt_state(authority_writer()),
        Some(FreezeReceiptState::Missing)
    );
    drop(log);

    let mut wrong_key = reopen(
        &setup.temp,
        7,
        authority_writer(),
        authority_writer(),
        key(99),
    );
    assert!(matches!(
        wrong_key.publish_local_freeze_receipt(digest),
        Err(PublicationError::FreezeReceiptDenied)
    ));
    drop(wrong_key);
    let mut wrong_writer = reopen(
        &setup.temp,
        7,
        authority_writer(),
        reader_writer(),
        authority_key(),
    );
    assert!(matches!(
        wrong_writer.publish_local_freeze_receipt(digest),
        Err(PublicationError::FreezeReceiptDenied)
    ));

    let mut aborted = authority_survivor_setup(64);
    let aborted_digest = aborted.proposal.digest().to_bytes();
    let abort_record =
        encode_signed_freeze_abort(&authority_key(), &aborted.proposal).expect("signed abort");
    aborted
        .log
        .ingest(&event(EventKind::FreezeAbort, &abort_record))
        .expect("abort append");
    assert!(matches!(
        aborted.log.publish_local_freeze_receipt(aborted_digest),
        Err(PublicationError::FreezeReceiptDenied)
    ));
}
