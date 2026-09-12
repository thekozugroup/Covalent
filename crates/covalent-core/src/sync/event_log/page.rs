//! Bounded, authenticated reads of committed folder-event history.
//!
//! Cursors are local capabilities for this exact open handle, never wire tokens
//! or peer acknowledgements. Page contents still require live peer permission
//! before transmission. Apply journals cannot be read through this API.

use std::fmt;
use std::sync::Arc;

use super::super::log_frame::{DecodedLogFrame, LogFileKind};
use super::{
    DurableEventLog, EventLogError, EventMachine, LogInner, LogIo, LogOrdinal,
    MAX_ENCODED_LOG_FRAME_BYTES, MAX_LOG_PLAINTEXT_BYTES, ParseOutcome, PreparedEvent,
    SYNC_LOG_FILE_MAGIC, log_frame,
};

/// Hard record-count cap for a single page, including tiny events.
pub const MAX_EVENT_PAGE_RECORDS: usize = 256;
/// Hard plaintext-byte cap for retained page contents, excluding bounded
/// record metadata and one frame's read/decryption scratch space.
pub const MAX_EVENT_PAGE_BYTES: usize = 4 * 1024 * 1024;

/// Requested per-page bounds. Bytes must allow at least one largest legal
/// event, ensuring a valid non-final page always makes progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventLogPageLimits {
    pub maximum_records: usize,
    pub maximum_plaintext_bytes: usize,
}

impl EventLogPageLimits {
    fn validate(self) -> Result<(), EventLogError> {
        if !(1..=MAX_EVENT_PAGE_RECORDS).contains(&self.maximum_records)
            || !(MAX_LOG_PLAINTEXT_BYTES..=MAX_EVENT_PAGE_BYTES)
                .contains(&self.maximum_plaintext_bytes)
        {
            return Err(EventLogError::InvalidPageLimits);
        }
        Ok(())
    }
}

/// Opaque position in one open log. Clone to retry a read; reopening a log
/// invalidates every old cursor even if all namespace IDs remain identical.
#[derive(Clone)]
pub struct EventLogCursor {
    token: Arc<()>,
    offset: u64,
    preceding_records: u64,
}

impl fmt::Debug for EventLogCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventLogCursor")
            .field("preceding_records", &self.preceding_records)
            .finish_non_exhaustive()
    }
}

/// One authenticated frame that exactly matches already accepted history.
/// Local ordinals do not represent a remote acknowledgement or applied state.
pub struct CommittedEvent {
    ordinal: u64,
    frame: DecodedLogFrame,
}

impl CommittedEvent {
    /// Returns the local encrypted-log ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }

    /// Borrows bounded plaintext. Its storage is zeroized on drop.
    #[must_use]
    pub fn event_bytes(&self) -> &[u8] {
        self.frame.plaintext()
    }
}

impl fmt::Debug for CommittedEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommittedEvent")
            .field("ordinal", &self.ordinal)
            .field("plaintext_bytes", &self.event_bytes().len())
            .finish_non_exhaustive()
    }
}

/// A bounded page and its continuation position. `at_end` describes this call's
/// committed prefix; subsequent appends can be read using the returned cursor.
pub struct EventLogPage {
    events: Vec<CommittedEvent>,
    next_cursor: EventLogCursor,
    at_end: bool,
    plaintext_bytes: usize,
}

impl EventLogPage {
    #[must_use]
    pub fn events(&self) -> &[CommittedEvent] {
        &self.events
    }

    #[must_use]
    pub const fn at_end(&self) -> bool {
        self.at_end
    }

    #[must_use]
    pub const fn plaintext_bytes(&self) -> usize {
        self.plaintext_bytes
    }

    #[must_use]
    pub fn next_cursor(&self) -> &EventLogCursor {
        &self.next_cursor
    }

    /// Moves the page contents and continuation without cloning plaintext.
    #[must_use]
    pub fn into_parts(self) -> (Vec<CommittedEvent>, EventLogCursor, bool) {
        (self.events, self.next_cursor, self.at_end)
    }
}

impl fmt::Debug for EventLogPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventLogPage")
            .field("event_count", &self.events.len())
            .field("plaintext_bytes", &self.plaintext_bytes)
            .field("at_end", &self.at_end)
            .finish_non_exhaustive()
    }
}

impl<M: EventMachine> DurableEventLog<M> {
    /// Starts a local history read. The first page revalidates the file and
    /// lifetime lock; creating a cursor alone performs no I/O or authorization.
    pub fn start_cursor(&self) -> Result<EventLogCursor, EventLogError> {
        self.inner.start_cursor()
    }

    /// Reads at most the requested count/bytes without rescanning older frames.
    /// A frame is returned only if authenticated and classified as an exact
    /// duplicate by the replay machine. Any I/O, authentication or acceptance
    /// mismatch poisons this handle, and no partial page escapes. Peer permission
    /// must be checked separately at transmission time, including after awaits.
    pub fn read_page(
        &mut self,
        cursor: &EventLogCursor,
        limits: EventLogPageLimits,
    ) -> Result<EventLogPage, EventLogError> {
        self.inner.read_page(cursor, limits)
    }
}

impl<M: EventMachine, I: LogIo> LogInner<M, I> {
    fn require_page_kind(&self) -> Result<(), EventLogError> {
        self.ensure_usable()?;
        if self.binding.file_kind() != LogFileKind::FolderEvents {
            return Err(EventLogError::WrongPageKind);
        }
        Ok(())
    }

    pub(super) fn start_cursor(&self) -> Result<EventLogCursor, EventLogError> {
        self.require_page_kind()?;
        Ok(EventLogCursor {
            token: Arc::clone(&self.cursor_token),
            offset: SYNC_LOG_FILE_MAGIC.len() as u64,
            preceding_records: 0,
        })
    }

    pub(super) fn read_page(
        &mut self,
        cursor: &EventLogCursor,
        limits: EventLogPageLimits,
    ) -> Result<EventLogPage, EventLogError> {
        self.require_page_kind()?;
        limits.validate()?;
        if !Arc::ptr_eq(&cursor.token, &self.cursor_token)
            || !(SYNC_LOG_FILE_MAGIC.len() as u64..=self.committed_bytes).contains(&cursor.offset)
            || cursor.preceding_records > self.records
        {
            return Err(EventLogError::InvalidCursor);
        }
        let mut events = Vec::new();
        events
            .try_reserve_exact(limits.maximum_records)
            .map_err(|_| EventLogError::QuotaExceeded)?;
        // This is read-only but a detected change invalidates accepted state.
        // Mark unusable until every check succeeds; errors expose no page.
        self.poisoned = true;
        let result = self.read_page_checked(cursor, limits, events)?;
        self.poisoned = false;
        Ok(result)
    }

    fn read_page_checked(
        &self,
        cursor: &EventLogCursor,
        limits: EventLogPageLimits,
        mut events: Vec<CommittedEvent>,
    ) -> Result<EventLogPage, EventLogError> {
        if self.io.len()? != self.committed_bytes {
            return Err(EventLogError::Changed);
        }
        if self.io.read(0, SYNC_LOG_FILE_MAGIC.len() as u64)? != SYNC_LOG_FILE_MAGIC {
            return Err(EventLogError::InvalidFileHeader);
        }
        let mut offset = cursor.offset;
        let mut preceding_records = cursor.preceding_records;
        let mut plaintext_bytes = 0_usize;
        while offset < self.committed_bytes && events.len() < limits.maximum_records {
            let requested = (self.committed_bytes - offset).min(MAX_ENCODED_LOG_FRAME_BYTES as u64);
            let input = self.io.read(offset, requested)?;
            if input.len() as u64 != requested {
                return Err(EventLogError::Changed);
            }
            let ordinal = preceding_records
                .checked_add(1)
                .filter(|ordinal| *ordinal <= self.records)
                .ok_or(EventLogError::Changed)?;
            let ParseOutcome::Complete(frame) =
                log_frame::parse_one(&input, &self.binding, LogOrdinal::new(ordinal)?, &self.key)?
            else {
                // Only open/replay can repair an incomplete tail. A cursor is
                // limited to this handle's already committed complete frames.
                return Err(EventLogError::Changed);
            };
            let next_plaintext_bytes = plaintext_bytes
                .checked_add(frame.plaintext().len())
                .ok_or(EventLogError::Changed)?;
            if next_plaintext_bytes > limits.maximum_plaintext_bytes {
                break;
            }
            if !matches!(
                self.machine.prepare(frame.plaintext()),
                Ok(PreparedEvent::Duplicate)
            ) {
                return Err(EventLogError::UnretainedEvent);
            }
            offset = offset
                .checked_add(frame.consumed() as u64)
                .filter(|offset| *offset <= self.committed_bytes)
                .ok_or(EventLogError::Changed)?;
            preceding_records = ordinal;
            plaintext_bytes = next_plaintext_bytes;
            events.push(CommittedEvent { ordinal, frame });
        }
        if self.io.len()? != self.committed_bytes
            || (offset == self.committed_bytes) != (preceding_records == self.records)
        {
            return Err(EventLogError::Changed);
        }
        Ok(EventLogPage {
            events,
            next_cursor: EventLogCursor {
                token: Arc::clone(&self.cursor_token),
                offset,
                preceding_records,
            },
            at_end: offset == self.committed_bytes,
            plaintext_bytes,
        })
    }
}
