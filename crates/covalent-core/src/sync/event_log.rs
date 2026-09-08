//! Encrypted, bounded append/replay storage for one private folder event stream.
//!
//! This layer proves a successful append reached file sync and replay. It does
//! not itself authorize a record, acknowledge a peer, or apply a user file. The
//! supplied machine must validate signatures, membership, causal dependencies,
//! canonical event bounds, and its own memory quota before accepting an event.
//! A single lifetime folder lock serializes cooperative writers. Whole-state
//! rollback and hostile same-user writers remain outside that lock's guarantee.

use thiserror::Error;

use super::log_frame::{
    self, LogBinding, LogFrameError, LogFrameKey, LogOrdinal, MAX_ENCODED_LOG_FRAME_BYTES,
    MAX_LOG_PLAINTEXT_BYTES, ParseOutcome, SYNC_LOG_FILE_MAGIC,
};
use super::state_dir::{
    PrivateStateDir, PrivateStateFile, PrivateStateLock, StateDirError, StateKey,
};

/// Disk and record bounds for an entire retained event history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventLogLimits {
    pub maximum_bytes: u64,
    pub maximum_records: u64,
}

/// A fixed, redacted rejection from the event-specific state machine.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("folder event validation failed")]
pub struct EventValidationError;

/// A non-mutating preflight result. Duplicate is permitted only for an exact
/// previously replayed event; it never appends another frame.
pub enum PreparedEvent<P> {
    Append(P),
    Duplicate,
}

/// Event authorization and deterministic replay supplied by the folder engine.
///
/// `prepare` must not mutate accepted state or expose side effects. It must
/// validate against the machine's complete current history and check resource
/// bounds before returning. `commit` applies that same prepared transition.
/// Neither method may publish an acknowledgement, sign a new operation, or
/// mutate user files. An error from `commit` poisons a live log until reopen.
/// Replay calls the same pair, in frame order, starting with a fresh machine.
pub trait EventMachine {
    type Prepared;

    fn prepare(&self, event: &[u8]) -> Result<PreparedEvent<Self::Prepared>, EventValidationError>;
    fn commit(&mut self, prepared: Self::Prepared) -> Result<(), EventValidationError>;
}

/// Successful storage outcome; neither variant is a peer acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventAppendOutcome {
    Committed { ordinal: u64 },
    Duplicate,
}

/// Fixed storage/validation errors contain no event, key, or user path bytes.
#[derive(Debug, Error)]
pub enum EventLogError {
    #[error("folder event log limits cannot contain the file header")]
    InvalidLimits,
    #[error("folder event log exceeds its configured quota")]
    QuotaExceeded,
    #[error("folder event log header is invalid or incomplete")]
    InvalidFileHeader,
    #[error("folder event log changed outside its transaction")]
    Changed,
    #[error("folder event log requires reopen after an uncertain transaction")]
    Poisoned,
    #[error(transparent)]
    State(#[from] StateDirError),
    #[error(transparent)]
    Frame(#[from] LogFrameError),
    #[error(transparent)]
    Validation(#[from] EventValidationError),
}

/// One lifetime-locked encrypted stream and its replay-derived machine.
pub struct DurableEventLog<M: EventMachine> {
    inner: LogInner<M, OsLogIo>,
}

impl<M: EventMachine> DurableEventLog<M> {
    /// Exclusively creates a new stream and syncs its header and directory.
    /// An incumbent is never replaced. A failed creation leaves uncertain state
    /// for explicit recovery; this method never deletes or silently recreates it.
    pub fn create(
        directory: &PrivateStateDir,
        file_name: &StateKey,
        binding: LogBinding,
        key: LogFrameKey,
        limits: EventLogLimits,
        machine: M,
    ) -> Result<Self, EventLogError> {
        validate_limits(limits)?;
        let lock = directory.try_lock()?;
        let file = directory.create_new_file(
            &lock,
            file_name,
            SYNC_LOG_FILE_MAGIC,
            limits.maximum_bytes,
        )?;
        let io = OsLogIo { file, lock };
        Ok(Self {
            inner: LogInner::open(io, binding, key, limits, machine)?,
        })
    }

    /// Replays an existing stream under its exclusive folder lock.
    /// Only a structurally incomplete final frame at physical EOF is repaired.
    /// Complete corruption or event rejection leaves the file untouched.
    pub fn open(
        directory: &PrivateStateDir,
        file_name: &StateKey,
        binding: LogBinding,
        key: LogFrameKey,
        limits: EventLogLimits,
        machine: M,
    ) -> Result<Self, EventLogError> {
        validate_limits(limits)?;
        let lock = directory.try_lock()?;
        let file = directory.open_file(file_name, limits.maximum_bytes)?;
        // A prior creation may have stopped before its parent sync. Persist
        // the admitted entry before this reopen can expose durable state.
        directory.sync(&lock)?;
        let io = OsLogIo { file, lock };
        Ok(Self {
            inner: LogInner::open(io, binding, key, limits, machine)?,
        })
    }

    /// Validates, appends, syncs, then commits an event to derived state.
    /// Any uncertain write, sync, or post-sync transition error poisons this
    /// handle. Reopen with a fresh machine before any retry or acknowledgement.
    pub fn append(&mut self, event: &[u8]) -> Result<EventAppendOutcome, EventLogError> {
        self.inner.append(event)
    }

    /// Returns only successfully replayed state from a usable handle.
    pub fn machine(&self) -> Result<&M, EventLogError> {
        self.inner.machine()
    }

    /// Number of committed frames replayed by this handle.
    pub fn committed_records(&self) -> Result<u64, EventLogError> {
        self.inner.ensure_usable()?;
        Ok(self.inner.records)
    }

    /// Number of bytes durably removed from a final incomplete frame on open.
    #[must_use]
    pub const fn repaired_tail_bytes(&self) -> u64 {
        self.inner.repaired_tail_bytes
    }
}

trait LogIo {
    fn len(&self) -> Result<u64, EventLogError>;
    fn read(&self, offset: u64, maximum: u64) -> Result<Vec<u8>, EventLogError>;
    fn append(&mut self, bytes: &[u8]) -> Result<(), EventLogError>;
    fn sync(&mut self) -> Result<(), EventLogError>;
    fn truncate(&mut self, length: u64) -> Result<(), EventLogError>;
}

struct OsLogIo {
    file: PrivateStateFile,
    lock: PrivateStateLock,
}

impl LogIo for OsLogIo {
    fn len(&self) -> Result<u64, EventLogError> {
        self.lock.validate()?;
        Ok(self.file.len()?)
    }
    fn read(&self, offset: u64, maximum: u64) -> Result<Vec<u8>, EventLogError> {
        self.lock.validate()?;
        Ok(self.file.read_range(offset, maximum)?)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), EventLogError> {
        Ok(self.file.append(&self.lock, bytes)?)
    }
    fn sync(&mut self) -> Result<(), EventLogError> {
        Ok(self.file.sync_all(&self.lock)?)
    }
    fn truncate(&mut self, length: u64) -> Result<(), EventLogError> {
        Ok(self.file.truncate_tail(&self.lock, length)?)
    }
}

struct LogInner<M: EventMachine, I: LogIo> {
    io: I,
    binding: LogBinding,
    key: LogFrameKey,
    limits: EventLogLimits,
    machine: M,
    records: u64,
    committed_bytes: u64,
    repaired_tail_bytes: u64,
    poisoned: bool,
}

fn validate_limits(limits: EventLogLimits) -> Result<(), EventLogError> {
    if limits.maximum_bytes < SYNC_LOG_FILE_MAGIC.len() as u64 {
        return Err(EventLogError::InvalidLimits);
    }
    Ok(())
}

impl<M: EventMachine, I: LogIo> LogInner<M, I> {
    fn open(
        mut io: I,
        binding: LogBinding,
        key: LogFrameKey,
        limits: EventLogLimits,
        mut machine: M,
    ) -> Result<Self, EventLogError> {
        validate_limits(limits)?;
        let initial_length = io.len()?;
        if initial_length > limits.maximum_bytes {
            return Err(EventLogError::QuotaExceeded);
        }
        if io.read(0, SYNC_LOG_FILE_MAGIC.len() as u64)? != SYNC_LOG_FILE_MAGIC {
            return Err(EventLogError::InvalidFileHeader);
        }
        let mut committed_bytes = SYNC_LOG_FILE_MAGIC.len() as u64;
        let mut records = 0_u64;
        let mut repaired_tail_bytes = 0;
        while committed_bytes < initial_length {
            let ordinal = records.checked_add(1).ok_or(EventLogError::QuotaExceeded)?;
            let requested =
                (initial_length - committed_bytes).min(MAX_ENCODED_LOG_FRAME_BYTES as u64);
            let input = io.read(committed_bytes, requested)?;
            if input.len() as u64 != requested || io.len()? != initial_length {
                return Err(EventLogError::Changed);
            }
            match log_frame::parse_one(&input, &binding, LogOrdinal::new(ordinal)?, &key)? {
                ParseOutcome::Complete(frame) => {
                    if ordinal > limits.maximum_records {
                        return Err(EventLogError::QuotaExceeded);
                    }
                    let PreparedEvent::Append(prepared) = machine.prepare(frame.plaintext())?
                    else {
                        // Our append path never writes duplicates. A different
                        // replay interpretation cannot silently discard a frame.
                        return Err(EventValidationError.into());
                    };
                    machine.commit(prepared)?;
                    committed_bytes = committed_bytes
                        .checked_add(frame.consumed() as u64)
                        .ok_or(EventLogError::QuotaExceeded)?;
                    records = ordinal;
                }
                ParseOutcome::Incomplete { .. } => {
                    // A complete legal frame always fits the bounded read.
                    // Incomplete may be repaired only at the captured EOF.
                    if committed_bytes + input.len() as u64 != initial_length {
                        return Err(EventLogError::Changed);
                    }
                    io.truncate(committed_bytes)?;
                    repaired_tail_bytes = initial_length - committed_bytes;
                    break;
                }
            }
        }
        if io.len()? != committed_bytes {
            return Err(EventLogError::Changed);
        }
        // A complete frame may come from an uncertain previous sync. Replay
        // alone is not durability: stabilize the accepted prefix, even when
        // there was no incomplete tail, before exposing duplicate receipts.
        io.sync()?;
        if io.len()? != committed_bytes {
            return Err(EventLogError::Changed);
        }
        Ok(Self {
            io,
            binding,
            key,
            limits,
            machine,
            records,
            committed_bytes,
            repaired_tail_bytes,
            poisoned: false,
        })
    }

    fn ensure_usable(&self) -> Result<(), EventLogError> {
        if self.poisoned {
            Err(EventLogError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn machine(&self) -> Result<&M, EventLogError> {
        self.ensure_usable()?;
        Ok(&self.machine)
    }

    fn append(&mut self, event: &[u8]) -> Result<EventAppendOutcome, EventLogError> {
        self.ensure_usable()?;
        // Even a duplicate must not be acknowledged through a replaced lock or
        // file, nor after observed uncoordinated growth/truncation.
        match self.io.len() {
            Ok(length) if length == self.committed_bytes => {}
            other => {
                self.poisoned = true;
                return Err(other.err().unwrap_or(EventLogError::Changed));
            }
        }
        if event.len() > MAX_LOG_PLAINTEXT_BYTES {
            return Err(EventLogError::QuotaExceeded);
        }
        let PreparedEvent::Append(prepared) = self.machine.prepare(event)? else {
            return Ok(EventAppendOutcome::Duplicate);
        };
        let ordinal = self
            .records
            .checked_add(1)
            .ok_or(EventLogError::QuotaExceeded)?;
        if ordinal > self.limits.maximum_records {
            return Err(EventLogError::QuotaExceeded);
        }
        let frame =
            log_frame::encode_frame(&self.binding, LogOrdinal::new(ordinal)?, &self.key, event)?;
        let next_bytes = self
            .committed_bytes
            .checked_add(frame.as_bytes().len() as u64)
            .filter(|length| *length <= self.limits.maximum_bytes)
            .ok_or(EventLogError::QuotaExceeded)?;
        self.poisoned = true;
        self.io.append(frame.as_bytes())?;
        self.io.sync()?;
        if self.io.len()? != next_bytes {
            return Err(EventLogError::Changed);
        }
        self.machine.commit(prepared)?;
        self.committed_bytes = next_bytes;
        self.records = ordinal;
        self.poisoned = false;
        Ok(EventAppendOutcome::Committed { ordinal })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::rc::Rc;

    use super::*;
    use crate::sync::ids::FolderId;
    use crate::sync::log_frame::LogFileKind;
    use uuid::Uuid;

    #[derive(Default)]
    struct Machine {
        records: Vec<Vec<u8>>,
        fail_commit: bool,
    }

    impl EventMachine for Machine {
        type Prepared = Vec<u8>;

        fn prepare(&self, event: &[u8]) -> Result<PreparedEvent<Vec<u8>>, EventValidationError> {
            let (&sequence, _) = event.split_first().ok_or(EventValidationError)?;
            if sequence == 0 {
                return Err(EventValidationError);
            }
            let index = usize::from(sequence) - 1;
            if let Some(existing) = self.records.get(index) {
                return if existing == event {
                    Ok(PreparedEvent::Duplicate)
                } else {
                    Err(EventValidationError)
                };
            }
            if index != self.records.len() {
                return Err(EventValidationError);
            }
            Ok(PreparedEvent::Append(event.to_vec()))
        }

        fn commit(&mut self, event: Vec<u8>) -> Result<(), EventValidationError> {
            self.records.push(event);
            if self.fail_commit {
                Err(EventValidationError)
            } else {
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct MemoryDisk {
        bytes: Vec<u8>,
        synced: Vec<u8>,
        writes: usize,
        truncations: usize,
        largest_read: u64,
        fail_after_append_bytes: Option<usize>,
        fail_sync: bool,
    }

    #[derive(Clone)]
    struct MemoryIo(Rc<RefCell<MemoryDisk>>);

    impl MemoryIo {
        fn from_bytes(bytes: Vec<u8>) -> Self {
            Self(Rc::new(RefCell::new(MemoryDisk {
                synced: bytes.clone(),
                bytes,
                ..MemoryDisk::default()
            })))
        }
        fn empty() -> Self {
            Self::from_bytes(SYNC_LOG_FILE_MAGIC.to_vec())
        }
        fn bytes(&self) -> Vec<u8> {
            self.0.borrow().bytes.clone()
        }
    }

    impl LogIo for MemoryIo {
        fn len(&self) -> Result<u64, EventLogError> {
            Ok(self.0.borrow().bytes.len() as u64)
        }
        fn read(&self, offset: u64, maximum: u64) -> Result<Vec<u8>, EventLogError> {
            let mut disk = self.0.borrow_mut();
            disk.largest_read = disk.largest_read.max(maximum);
            let start = (offset as usize).min(disk.bytes.len());
            let end = start.saturating_add(maximum as usize).min(disk.bytes.len());
            Ok(disk.bytes[start..end].to_vec())
        }
        fn append(&mut self, bytes: &[u8]) -> Result<(), EventLogError> {
            let mut disk = self.0.borrow_mut();
            disk.writes += 1;
            if let Some(count) = disk.fail_after_append_bytes.take() {
                disk.bytes
                    .extend_from_slice(&bytes[..count.min(bytes.len())]);
                return Err(EventLogError::Changed);
            }
            disk.bytes.extend_from_slice(bytes);
            Ok(())
        }
        fn sync(&mut self) -> Result<(), EventLogError> {
            let mut disk = self.0.borrow_mut();
            if disk.fail_sync {
                return Err(EventLogError::Changed);
            }
            disk.synced = disk.bytes.clone();
            Ok(())
        }
        fn truncate(&mut self, length: u64) -> Result<(), EventLogError> {
            let mut disk = self.0.borrow_mut();
            disk.truncations += 1;
            disk.bytes.truncate(length as usize);
            Ok(())
        }
    }

    fn binding() -> LogBinding {
        LogBinding::new(
            FolderId::from_uuid(Uuid::from_u128(1)),
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            LogFileKind::FolderEvents,
        )
    }
    fn key() -> LogFrameKey {
        LogFrameKey::from_bytes([7; 32])
    }
    fn limits() -> EventLogLimits {
        EventLogLimits {
            maximum_bytes: 1 << 20,
            maximum_records: 128,
        }
    }
    fn event(n: u8) -> Vec<u8> {
        let mut bytes = vec![n];
        bytes.extend_from_slice(format!("private-event-canary-{n}").as_bytes());
        bytes
    }
    fn frame(n: u64, bytes: &[u8]) -> Vec<u8> {
        log_frame::encode_frame(&binding(), LogOrdinal::new(n).unwrap(), &key(), bytes)
            .unwrap()
            .as_bytes()
            .to_vec()
    }
    fn open(io: MemoryIo) -> Result<LogInner<Machine, MemoryIo>, EventLogError> {
        LogInner::open(io, binding(), key(), limits(), Machine::default())
    }

    #[test]
    fn success_syncs_before_return_and_exact_duplicate_needs_no_quota_growth() {
        let io = MemoryIo::empty();
        let exact_size = 8 + frame(1, &event(1)).len() as u64;
        let bounds = EventLogLimits {
            maximum_bytes: exact_size,
            maximum_records: 1,
        };
        let mut log =
            LogInner::open(io.clone(), binding(), key(), bounds, Machine::default()).unwrap();
        assert_eq!(
            log.append(&event(1)).unwrap(),
            EventAppendOutcome::Committed { ordinal: 1 }
        );
        assert_eq!(io.0.borrow().synced, io.bytes());
        assert!(
            !io.bytes()
                .windows(b"private-event-canary".len())
                .any(|window| window == b"private-event-canary")
        );
        assert_eq!(
            log.append(&event(1)).unwrap(),
            EventAppendOutcome::Duplicate
        );
        assert!(matches!(
            log.append(&event(2)),
            Err(EventLogError::QuotaExceeded)
        ));
        assert_eq!(io.0.borrow().writes, 1);
        let replay =
            LogInner::open(io.clone(), binding(), key(), bounds, Machine::default()).unwrap();
        assert_eq!(replay.machine().unwrap().records, vec![event(1)]);
        assert!(io.0.borrow().largest_read <= MAX_ENCODED_LOG_FRAME_BYTES as u64);
    }

    #[test]
    fn rejection_and_oversize_do_not_write_or_poison_a_valid_handle() {
        let io = MemoryIo::empty();
        let mut log = open(io.clone()).unwrap();
        assert!(matches!(
            log.append(&event(2)),
            Err(EventLogError::Validation(_))
        ));
        assert!(matches!(
            log.append(&vec![1; MAX_LOG_PLAINTEXT_BYTES + 1]),
            Err(EventLogError::QuotaExceeded)
        ));
        assert_eq!(io.0.borrow().writes, 0);
        assert!(log.machine().unwrap().records.is_empty());
        assert!(log.append(&event(1)).is_ok());
    }

    #[test]
    fn every_valid_incomplete_final_prefix_repairs_only_the_tail() {
        let mut committed = SYNC_LOG_FILE_MAGIC.to_vec();
        committed.extend(frame(1, &event(1)));
        let second = frame(2, &event(2));
        for prefix in 0..second.len() {
            let mut bytes = committed.clone();
            bytes.extend_from_slice(&second[..prefix]);
            let io = MemoryIo::from_bytes(bytes);
            let replay =
                open(io.clone()).unwrap_or_else(|error| panic!("prefix {prefix}: {error}"));
            assert_eq!(io.bytes(), committed);
            assert_eq!(io.0.borrow().synced, committed);
            assert_eq!(replay.repaired_tail_bytes, prefix as u64);
            assert_eq!(replay.machine().unwrap().records, vec![event(1)]);
        }
    }

    #[test]
    fn complete_tail_or_middle_corruption_is_never_truncated() {
        let first = frame(1, &event(1));
        let second = frame(2, &event(2));
        let mut complete = SYNC_LOG_FILE_MAGIC.to_vec();
        complete.extend(&first);
        complete.extend(&second);
        for offset in [
            8,
            8 + 23,
            8 + first.len() - 1,
            8 + first.len() + 47,
            complete.len() - 1,
        ] {
            let mut changed = complete.clone();
            changed[offset] ^= 0x80;
            let io = MemoryIo::from_bytes(changed.clone());
            assert!(open(io.clone()).is_err());
            assert_eq!(io.bytes(), changed);
            assert_eq!(io.0.borrow().truncations, 0);
        }
    }

    #[test]
    fn encrypted_but_invalid_event_or_duplicate_frame_halts_replay_without_repair() {
        for invalid in [event(1), event(3)] {
            let mut bytes = SYNC_LOG_FILE_MAGIC.to_vec();
            bytes.extend(frame(1, &event(1)));
            bytes.extend(frame(2, &invalid));
            let io = MemoryIo::from_bytes(bytes.clone());
            assert!(matches!(
                open(io.clone()),
                Err(EventLogError::Validation(_))
            ));
            assert_eq!(io.bytes(), bytes);
            assert_eq!(io.0.borrow().truncations, 0);
        }
    }

    #[test]
    fn uncertain_append_always_poisons_and_reopen_recovers_exactly_once() {
        let length = frame(1, &event(1)).len();
        for count in [0, 1, 23, 47, length - 1, length] {
            let io = MemoryIo::empty();
            let mut log = open(io.clone()).unwrap();
            io.0.borrow_mut().fail_after_append_bytes = Some(count);
            assert!(log.append(&event(1)).is_err());
            assert!(matches!(log.machine(), Err(EventLogError::Poisoned)));
            assert!(matches!(
                log.append(&event(2)),
                Err(EventLogError::Poisoned)
            ));
            let mut replay = open(io.clone()).unwrap();
            assert_eq!(
                replay.machine().unwrap().records.len(),
                usize::from(count == length)
            );
            assert_eq!(
                replay.append(&event(1)).unwrap(),
                if count == length {
                    EventAppendOutcome::Duplicate
                } else {
                    EventAppendOutcome::Committed { ordinal: 1 }
                }
            );
            assert_eq!(replay.machine().unwrap().records, vec![event(1)]);
        }
    }

    #[test]
    fn failed_sync_and_failed_post_sync_commit_require_fresh_replay() {
        for fail_sync in [true, false] {
            let io = MemoryIo::empty();
            let mut log = open(io.clone()).unwrap();
            io.0.borrow_mut().fail_sync = fail_sync;
            log.machine.fail_commit = !fail_sync;
            assert!(log.append(&event(1)).is_err());
            assert!(matches!(log.machine(), Err(EventLogError::Poisoned)));
            io.0.borrow_mut().fail_sync = false;
            let mut replay = open(io).unwrap();
            assert_eq!(
                replay.append(&event(1)).unwrap(),
                EventAppendOutcome::Duplicate
            );
            assert_eq!(replay.machine().unwrap().records, vec![event(1)]);
        }
    }

    #[test]
    fn clean_reopen_must_sync_before_exposing_previously_uncertain_frames() {
        let io = MemoryIo::empty();
        let mut log = open(io.clone()).unwrap();
        io.0.borrow_mut().fail_sync = true;
        assert!(log.append(&event(1)).is_err());
        let unsynced = io.bytes();
        assert_ne!(io.0.borrow().synced, unsynced);
        assert!(open(io.clone()).is_err());
        assert_eq!(io.bytes(), unsynced);
        assert_eq!(io.0.borrow().truncations, 0);
        io.0.borrow_mut().fail_sync = false;
        let mut replay = open(io.clone()).unwrap();
        assert_eq!(io.0.borrow().synced, unsynced);
        assert_eq!(
            replay.append(&event(1)).unwrap(),
            EventAppendOutcome::Duplicate
        );
        assert_eq!(replay.records, 1);
    }

    #[test]
    fn byte_quota_at_one_less_than_frame_size_refuses_before_mutation() {
        let io = MemoryIo::empty();
        let bounds = EventLogLimits {
            maximum_bytes: 8 + frame(1, &event(1)).len() as u64 - 1,
            ..limits()
        };
        let mut log =
            LogInner::open(io.clone(), binding(), key(), bounds, Machine::default()).unwrap();
        assert!(matches!(
            log.append(&event(1)),
            Err(EventLogError::QuotaExceeded)
        ));
        assert_eq!(io.bytes(), SYNC_LOG_FILE_MAGIC);
        assert_eq!(io.0.borrow().writes, 0);
        assert!(log.machine().unwrap().records.is_empty());
    }

    #[test]
    fn external_length_change_poisoning_includes_duplicate_requests() {
        let io = MemoryIo::empty();
        let mut log = open(io.clone()).unwrap();
        log.append(&event(1)).unwrap();
        io.0.borrow_mut().bytes.push(0);
        assert!(matches!(log.append(&event(1)), Err(EventLogError::Changed)));
        assert!(matches!(log.machine(), Err(EventLogError::Poisoned)));
    }

    #[test]
    fn invalid_header_wrong_key_and_replay_quota_preserve_existing_bytes() {
        let mut complete = SYNC_LOG_FILE_MAGIC.to_vec();
        complete.extend(frame(1, &event(1)));
        for bytes in [Vec::new(), b"CVSLOG0".to_vec(), b"CVSLOG02".to_vec()] {
            let io = MemoryIo::from_bytes(bytes.clone());
            assert!(matches!(
                open(io.clone()),
                Err(EventLogError::InvalidFileHeader)
            ));
            assert_eq!(io.bytes(), bytes);
        }
        let io = MemoryIo::from_bytes(complete.clone());
        assert!(
            LogInner::open(
                io.clone(),
                binding(),
                LogFrameKey::from_bytes([8; 32]),
                limits(),
                Machine::default()
            )
            .is_err()
        );
        let bounds = EventLogLimits {
            maximum_records: 0,
            ..limits()
        };
        assert!(matches!(
            LogInner::open(io.clone(), binding(), key(), bounds, Machine::default()),
            Err(EventLogError::QuotaExceeded)
        ));
        assert_eq!(io.bytes(), complete);
        assert_eq!(io.0.borrow().truncations, 0);
    }

    #[test]
    fn real_private_log_retains_lock_reopens_and_never_clobbers_incumbent() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateStateDir::open_root(temp.path()).unwrap();
        let name = StateKey::new("events.v1").unwrap();
        let mut log = DurableEventLog::create(
            &directory,
            &name,
            binding(),
            key(),
            limits(),
            Machine::default(),
        )
        .unwrap();
        log.append(&event(1)).unwrap();
        assert!(matches!(
            DurableEventLog::open(
                &directory,
                &name,
                binding(),
                key(),
                limits(),
                Machine::default()
            ),
            Err(EventLogError::State(StateDirError::Locked))
        ));
        drop(log);
        assert!(matches!(
            DurableEventLog::create(
                &directory,
                &name,
                binding(),
                key(),
                limits(),
                Machine::default()
            ),
            Err(EventLogError::State(StateDirError::AlreadyExists))
        ));
        let mut reopened = DurableEventLog::open(
            &directory,
            &name,
            binding(),
            key(),
            limits(),
            Machine::default(),
        )
        .unwrap();
        assert_eq!(reopened.committed_records().unwrap(), 1);
        assert_eq!(
            reopened.append(&event(1)).unwrap(),
            EventAppendOutcome::Duplicate
        );
        assert_eq!(reopened.machine().unwrap().records, vec![event(1)]);
    }
}
