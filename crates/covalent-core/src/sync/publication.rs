//! Local operation publication through an exclusively held durable folder log.
//!
//! The writer's counter, predecessor and full causal context come from the
//! same replay-derived machine that validates the append. Signed bytes remain
//! private until the append and machine commit succeed. This layer does not
//! claim that file content is retained, applied, or acknowledged by a peer.

use std::fmt;

use ed25519_dalek::SigningKey;
use thiserror::Error;
use zeroize::Zeroizing;

use super::body::OperationBody;
use super::event::{EncodedEventEnvelope, EventEnvelope, EventKind};
use super::event_log::{DurableEventLog, EventAppendOutcome, EventLogError};
use super::ids::WriterId;
use super::log_frame::LogFileKind;
use super::machine::FolderEventMachine;
use super::operation::encode_signed_operation;
use super::register::OpId;

/// Fixed publication failures never include a key, record body, or user path.
#[derive(Debug, Error)]
pub enum PublicationError {
    #[error("folder publication log has the wrong namespace binding")]
    WrongLogBinding,
    #[error("local writer cannot publish in the current folder state")]
    NotWritable,
    #[error("local operation could not be encoded")]
    Encoding,
    #[error("local publication requires reopen after an invariant failure")]
    Invariant,
    #[error(transparent)]
    Log(#[from] EventLogError),
}

/// A signed local operation whose exact event has reached durable commit.
///
/// Possession is evidence of this log's append result, not of content transfer,
/// filesystem application, or a peer acknowledgement.
pub struct PublishedOperation {
    id: OpId,
    ordinal: u64,
    event: EncodedEventEnvelope,
}

impl PublishedOperation {
    /// Returns the committed folder-global writer dot.
    #[must_use]
    pub const fn id(&self) -> OpId {
        self.id
    }

    /// Returns the local encrypted log ordinal, not a remote receipt.
    #[must_use]
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }

    /// Returns the exact committed `kind || signed record` event bytes.
    #[must_use]
    pub fn event_bytes(&self) -> &[u8] {
        self.event.as_bytes()
    }
}

impl fmt::Debug for PublishedOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishedOperation")
            .field("id", &self.id)
            .field("ordinal", &self.ordinal)
            .field("event_length", &self.event.as_bytes().len())
            .finish()
    }
}

/// A local signing key and the one exclusively held folder log it may advance.
///
/// No signing or mutable-machine handle escapes. An inactive or joining writer
/// may ingest events, while each publication separately requires the current
/// accepted write grant and matching key. The caller must retain content before
/// publishing its reference and enforce live peer authorization before ingress.
pub struct DurableFolderLog {
    log: DurableEventLog<FolderEventMachine>,
    writer: WriterId,
    key: SigningKey,
    invariant_failed: bool,
}

impl DurableFolderLog {
    /// Takes ownership of a usable, correctly bound folder-event log and key.
    /// Joining and read-only writers are allowed; this is not a write grant.
    pub fn new(
        log: DurableEventLog<FolderEventMachine>,
        writer: WriterId,
        key: SigningKey,
    ) -> Result<Self, PublicationError> {
        let binding = log.binding()?;
        if binding.file_kind() != LogFileKind::FolderEvents
            || binding.folder_id() != log.machine()?.folder_id()
        {
            return Err(PublicationError::WrongLogBinding);
        }
        Ok(Self {
            log,
            writer,
            key,
            invariant_failed: false,
        })
    }

    /// Returns accepted derived state only while the underlying log is usable.
    pub fn machine(&self) -> Result<&FolderEventMachine, PublicationError> {
        self.ensure_usable()?;
        Ok(self.log.machine()?)
    }

    /// Appends a received event after the concrete machine validates it.
    /// The result is storage evidence; live session permission and peer
    /// acknowledgement policy remain the surrounding runtime's responsibility.
    pub fn ingest(&mut self, event: &[u8]) -> Result<EventAppendOutcome, PublicationError> {
        self.ensure_usable()?;
        Ok(self.log.append(event)?)
    }

    /// Allocates and signs the next operation inside this log's transaction.
    /// Rejection or uncertain persistence returns no signed publication bytes.
    pub fn publish_local(
        &mut self,
        body: &OperationBody,
    ) -> Result<PublishedOperation, PublicationError> {
        self.ensure_usable()?;
        let context = self
            .log
            .machine()?
            .publication_context(self.writer, &self.key.verifying_key())
            .map_err(|_| PublicationError::NotWritable)?;
        let id = OpId::new(self.writer.into_vector_actor(), context.counter())
            .map_err(|_| PublicationError::Encoding)?;
        let body_bytes = Zeroizing::new(body.encode());
        let record = Zeroizing::new(
            encode_signed_operation(
                &self.key,
                context.folder_id(),
                context.writer_id(),
                context.membership_epoch(),
                context.membership_epoch_digest(),
                context.counter(),
                context.predecessor(),
                context.clock_entries(),
                &body_bytes,
            )
            .map_err(|_| PublicationError::Encoding)?,
        );
        let event = EventEnvelope::from_signed_record(EventKind::Operation, &record)
            .and_then(|envelope| envelope.encode())
            .map_err(|_| PublicationError::Encoding)?;
        match self.log.append(event.as_bytes())? {
            EventAppendOutcome::Committed { ordinal } => {
                Ok(PublishedOperation { id, ordinal, event })
            }
            EventAppendOutcome::Duplicate => {
                // A freshly derived next counter cannot already be committed.
                // Do not expose the signed bytes or continue this wrapper.
                self.invariant_failed = true;
                Err(PublicationError::Invariant)
            }
        }
    }

    fn ensure_usable(&self) -> Result<(), PublicationError> {
        if self.invariant_failed {
            return Err(PublicationError::Invariant);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    use covalent_protocol::DeviceId;
    use uuid::Uuid;

    use super::*;
    use crate::sync::body::EntryValue;
    use crate::sync::event_log::{EventLogLimits, EventMachine, PreparedEvent};
    use crate::sync::ids::FolderId;
    use crate::sync::log_frame::{LogBinding, LogFrameKey};
    use crate::sync::machine::{FolderMachineConfig, FolderMachineLimits};
    use crate::sync::membership::{MemberGrant, MemberRole, encode_signed_epoch};
    use crate::sync::operation::decode_signature_checked_operation;
    use crate::sync::path::SyncPath;
    use crate::sync::state_dir::{PrivateStateDir, StateKey};

    fn folder() -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(81))
    }

    fn writer() -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(82))
    }

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[83; 32])
    }

    fn machine(local_writer_id: WriterId) -> FolderEventMachine {
        FolderEventMachine::new(FolderMachineConfig {
            folder_id: folder(),
            authority_writer_id: writer(),
            pinned_authority_key: signing_key().verifying_key(),
            local_writer_id,
            limits: FolderMachineLimits {
                maximum_index_bytes: 1 << 20,
                maximum_operations: 128,
                maximum_paths: 128,
            },
        })
        .unwrap()
    }

    fn binding() -> LogBinding {
        LogBinding::new(
            folder(),
            Uuid::from_u128(84),
            Uuid::from_u128(85),
            LogFileKind::FolderEvents,
        )
    }

    fn frame_key() -> LogFrameKey {
        LogFrameKey::from_bytes([86; 32])
    }

    fn limits(maximum_records: u64) -> EventLogLimits {
        EventLogLimits {
            maximum_bytes: 1 << 20,
            maximum_records,
        }
    }

    fn name() -> StateKey {
        StateKey::new("events.v1").unwrap()
    }

    fn genesis() -> EncodedEventEnvelope {
        let grant = MemberGrant::new(
            writer(),
            signing_key().verifying_key(),
            DeviceId::from_uuid(Uuid::from_u128(87)),
            SigningKey::from_bytes(&[88; 32]).verifying_key(),
            MemberRole::ReadWrite,
        )
        .unwrap();
        let record = encode_signed_epoch(
            &signing_key(),
            folder(),
            1,
            writer(),
            None,
            &[grant],
            None,
            &[],
            &[],
            &[],
            &[],
        )
        .unwrap();
        EventEnvelope::from_signed_record(EventKind::MembershipEpoch, &record)
            .unwrap()
            .encode()
            .unwrap()
    }

    fn fixture(maximum_records: u64, with_genesis: bool) -> (tempfile::TempDir, DurableFolderLog) {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateStateDir::open_root(temp.path()).unwrap();
        let log = DurableEventLog::create(
            &directory,
            &name(),
            binding(),
            frame_key(),
            limits(maximum_records),
            machine(writer()),
        )
        .unwrap();
        let mut publisher = DurableFolderLog::new(log, writer(), signing_key()).unwrap();
        if with_genesis {
            publisher.ingest(genesis().as_bytes()).unwrap();
        }
        (temp, publisher)
    }

    fn reopen(temp: &tempfile::TempDir, maximum_records: u64) -> DurableFolderLog {
        let directory = PrivateStateDir::open_root(temp.path()).unwrap();
        let log = DurableEventLog::open(
            &directory,
            &name(),
            binding(),
            frame_key(),
            limits(maximum_records),
            machine(writer()),
        )
        .unwrap();
        DurableFolderLog::new(log, writer(), signing_key()).unwrap()
    }

    fn body(path: &str, value: EntryValue) -> OperationBody {
        OperationBody::new(SyncPath::from_wire(path).unwrap(), value)
    }

    #[test]
    fn cross_path_publications_replay_and_resume_one_global_author_chain() {
        let (temp, mut log) = fixture(128, true);
        let first = log
            .publish_local(&body("private-first-canary", EntryValue::Directory))
            .unwrap();
        let second = log
            .publish_local(&body("private-second-canary", EntryValue::Directory))
            .unwrap();
        assert_eq!((first.id().counter(), second.id().counter()), (1, 2));
        assert_eq!((first.ordinal(), second.ordinal()), (2, 3));
        let first_record = EventEnvelope::parse(first.event_bytes()).unwrap();
        let second_record = EventEnvelope::parse(second.event_bytes()).unwrap();
        let checked_first = decode_signature_checked_operation(
            first_record.record(),
            folder(),
            writer(),
            &signing_key().verifying_key(),
        )
        .unwrap();
        let checked_second = decode_signature_checked_operation(
            second_record.record(),
            folder(),
            writer(),
            &signing_key().verifying_key(),
        )
        .unwrap();
        assert_eq!(
            checked_second.header().predecessor(),
            Some(checked_first.digest().to_bytes())
        );
        assert_eq!(
            checked_second
                .header()
                .clock()
                .counter(writer().into_vector_actor()),
            2
        );
        assert!(!format!("{first:?}").contains("private-first-canary"));
        let encrypted = fs::read(temp.path().join("events.v1")).unwrap();
        assert!(
            !encrypted
                .windows(b"private-first-canary".len())
                .any(|w| w == b"private-first-canary")
        );
        drop(log);

        let mut log = reopen(&temp, 128);
        assert_eq!(
            log.ingest(first.event_bytes()).unwrap(),
            EventAppendOutcome::Duplicate
        );
        let third = log
            .publish_local(&body("private-first-canary", EntryValue::Tombstone))
            .unwrap();
        assert_eq!(third.id().counter(), 3);
        assert_eq!(third.ordinal(), 4);
        let record = EventEnvelope::parse(third.event_bytes()).unwrap();
        let checked = decode_signature_checked_operation(
            record.record(),
            folder(),
            writer(),
            &signing_key().verifying_key(),
        )
        .unwrap();
        assert_eq!(
            checked.header().predecessor(),
            Some(checked_second.digest().to_bytes())
        );
        let register = log
            .machine()
            .unwrap()
            .register(&SyncPath::from_wire("private-first-canary").unwrap())
            .unwrap();
        assert_eq!(register.active_count(), 1);
        assert_eq!(
            *register.active().next().unwrap().value(),
            EntryValue::Tombstone
        );
    }

    #[test]
    fn quota_rejection_returns_no_publication_and_does_not_consume_a_dot() {
        let (temp, mut log) = fixture(1, true);
        let original = fs::read(temp.path().join("events.v1")).unwrap();
        assert!(matches!(
            log.publish_local(&body("retained", EntryValue::Directory)),
            Err(PublicationError::Log(EventLogError::QuotaExceeded))
        ));
        assert_eq!(log.machine().unwrap().current_frontier().actor_count(), 0);
        assert_eq!(fs::read(temp.path().join("events.v1")).unwrap(), original);
        drop(log);
        let mut log = reopen(&temp, 128);
        let publication = log
            .publish_local(&body("retained", EntryValue::Directory))
            .unwrap();
        assert_eq!(publication.id().counter(), 1);
    }

    #[test]
    fn absent_grant_wrong_key_and_wrong_configured_writer_cannot_sign() {
        let (temp, mut log) = fixture(128, false);
        let original = fs::read(temp.path().join("events.v1")).unwrap();
        assert!(matches!(
            log.publish_local(&body("absent", EntryValue::Directory)),
            Err(PublicationError::NotWritable)
        ));
        assert_eq!(fs::read(temp.path().join("events.v1")).unwrap(), original);
        log.ingest(genesis().as_bytes()).unwrap();
        let DurableFolderLog { log, .. } = log;
        let mut wrong_key =
            DurableFolderLog::new(log, writer(), SigningKey::from_bytes(&[89; 32])).unwrap();
        assert!(matches!(
            wrong_key.publish_local(&body("wrong-key", EntryValue::Directory)),
            Err(PublicationError::NotWritable)
        ));
        drop(wrong_key);
        let directory = PrivateStateDir::open_root(temp.path()).unwrap();
        let replay = DurableEventLog::open(
            &directory,
            &name(),
            binding(),
            frame_key(),
            limits(128),
            machine(WriterId::from_uuid(Uuid::from_u128(90))),
        )
        .unwrap();
        let mut wrong_writer = DurableFolderLog::new(replay, writer(), signing_key()).unwrap();
        assert!(matches!(
            wrong_writer.publish_local(&body("wrong-writer", EntryValue::Directory)),
            Err(PublicationError::NotWritable)
        ));
        assert_eq!(
            wrong_writer
                .machine()
                .unwrap()
                .current_frontier()
                .actor_count(),
            0
        );
    }

    #[test]
    fn external_log_growth_rejects_publication_and_poisoned_state_access() {
        let (temp, mut log) = fixture(128, true);
        let mut external = OpenOptions::new()
            .append(true)
            .open(temp.path().join("events.v1"))
            .unwrap();
        external.write_all(b"external-change").unwrap();
        external.sync_all().unwrap();
        assert!(matches!(
            log.publish_local(&body("unpublished", EntryValue::Directory)),
            Err(PublicationError::Log(EventLogError::Changed))
        ));
        assert!(matches!(
            log.machine(),
            Err(PublicationError::Log(EventLogError::Poisoned))
        ));
        assert!(
            log.publish_local(&body("still-unpublished", EntryValue::Directory))
                .is_err()
        );
    }

    #[test]
    fn constructor_rejects_a_different_folder_or_apply_log_binding() {
        for wrong in [
            LogBinding::new(
                FolderId::from_uuid(Uuid::from_u128(91)),
                binding().installation_id(),
                binding().generation_id(),
                LogFileKind::FolderEvents,
            ),
            LogBinding::new(
                folder(),
                binding().installation_id(),
                binding().generation_id(),
                LogFileKind::Apply,
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let directory = PrivateStateDir::open_root(temp.path()).unwrap();
            let log = DurableEventLog::create(
                &directory,
                &name(),
                wrong,
                frame_key(),
                limits(128),
                machine(writer()),
            )
            .unwrap();
            assert!(matches!(
                DurableFolderLog::new(log, writer(), signing_key()),
                Err(PublicationError::WrongLogBinding)
            ));
        }
    }

    #[test]
    fn durable_create_and_open_reject_prepopulated_history_before_changing_files() {
        fn populated() -> FolderEventMachine {
            let mut state = machine(writer());
            let PreparedEvent::Append(prepared) = state.prepare(genesis().as_bytes()).unwrap()
            else {
                panic!("new genesis cannot be a duplicate");
            };
            state.commit(prepared).unwrap();
            assert!(!state.is_empty_for_replay());
            state
        }
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateStateDir::open_root(temp.path()).unwrap();
        assert!(matches!(
            DurableEventLog::create(
                &directory,
                &name(),
                binding(),
                frame_key(),
                limits(128),
                populated()
            ),
            Err(EventLogError::NonemptyReplayMachine)
        ));
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);

        let log = DurableEventLog::create(
            &directory,
            &name(),
            binding(),
            frame_key(),
            limits(128),
            machine(writer()),
        )
        .unwrap();
        drop(log);
        let original = fs::read(temp.path().join("events.v1")).unwrap();
        assert!(matches!(
            DurableEventLog::open(
                &directory,
                &name(),
                binding(),
                frame_key(),
                limits(128),
                populated()
            ),
            Err(EventLogError::NonemptyReplayMachine)
        ));
        assert_eq!(fs::read(temp.path().join("events.v1")).unwrap(), original);
    }
}
