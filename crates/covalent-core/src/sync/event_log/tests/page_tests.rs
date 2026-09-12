use super::*;

fn page_limits(records: usize, bytes: usize) -> EventLogPageLimits {
    EventLogPageLimits {
        maximum_records: records,
        maximum_plaintext_bytes: bytes,
    }
}

fn ordinary_limits() -> EventLogPageLimits {
    page_limits(2, MAX_LOG_PLAINTEXT_BYTES)
}

#[derive(Default)]
struct LongMachine(Vec<Vec<u8>>);

impl EventMachine for LongMachine {
    type Prepared = Vec<u8>;

    fn is_empty_for_replay(&self) -> bool {
        self.0.is_empty()
    }

    fn prepare(&self, event: &[u8]) -> Result<PreparedEvent<Self::Prepared>, EventValidationError> {
        let counter = u64::from_le_bytes(
            event
                .get(..8)
                .ok_or(EventValidationError)?
                .try_into()
                .unwrap(),
        );
        let index = usize::try_from(counter.checked_sub(1).ok_or(EventValidationError)?)
            .map_err(|_| EventValidationError)?;
        if let Some(existing) = self.0.get(index) {
            return if existing == event {
                Ok(PreparedEvent::Duplicate)
            } else {
                Err(EventValidationError)
            };
        }
        if index != self.0.len() {
            return Err(EventValidationError);
        }
        Ok(PreparedEvent::Append(event.to_vec()))
    }

    fn commit(&mut self, event: Self::Prepared) -> Result<(), EventValidationError> {
        self.0.push(event);
        Ok(())
    }
}

#[test]
fn hard_count_and_four_megabyte_bounds_split_history_without_losing_records() {
    for (count, length, expected_first_count) in [
        (257_u64, 8, MAX_EVENT_PAGE_RECORDS),
        (
            129,
            MAX_LOG_PLAINTEXT_BYTES,
            MAX_EVENT_PAGE_BYTES / MAX_LOG_PLAINTEXT_BYTES,
        ),
    ] {
        let io = MemoryIo::empty();
        let mut log = LogInner::open(
            io.clone(),
            binding(),
            key(),
            EventLogLimits {
                maximum_bytes: 16 * 1024 * 1024,
                maximum_records: 300,
            },
            LongMachine::default(),
        )
        .unwrap();
        for counter in 1..=count {
            let mut event = vec![37; length];
            event[..8].copy_from_slice(&counter.to_le_bytes());
            log.append(&event).unwrap();
        }
        let bounds = page_limits(MAX_EVENT_PAGE_RECORDS, MAX_EVENT_PAGE_BYTES);
        let first = log.read_page(&log.start_cursor().unwrap(), bounds).unwrap();
        assert_eq!(first.events().len(), expected_first_count);
        assert!(!first.at_end());
        assert!(first.plaintext_bytes() <= MAX_EVENT_PAGE_BYTES);
        let second = log.read_page(first.next_cursor(), bounds).unwrap();
        assert_eq!((first.events().len() + second.events().len()) as u64, count);
        assert!(second.at_end());
        assert_eq!(
            second.events()[0].ordinal(),
            expected_first_count as u64 + 1
        );
        assert!(io.0.borrow().largest_read <= MAX_ENCODED_LOG_FRAME_BYTES as u64);
    }
}

#[derive(Clone)]
struct ChangingReadIo {
    io: MemoryIo,
    change_on_read: Rc<std::cell::Cell<Option<usize>>>,
    reads: Rc<std::cell::Cell<usize>>,
    fail_read: bool,
}

impl LogIo for ChangingReadIo {
    fn len(&self) -> Result<u64, EventLogError> {
        self.io.len()
    }
    fn read(&self, offset: u64, maximum: u64) -> Result<Vec<u8>, EventLogError> {
        let bytes = self.io.read(offset, maximum)?;
        let count = self.reads.get() + 1;
        self.reads.set(count);
        if self.change_on_read.get() == Some(count) {
            if self.fail_read {
                return Err(EventLogError::Changed);
            }
            self.io.0.borrow_mut().bytes.push(0);
        }
        Ok(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), EventLogError> {
        self.io.append(bytes)
    }
    fn sync(&mut self) -> Result<(), EventLogError> {
        self.io.sync()
    }
    fn truncate(&mut self, length: u64) -> Result<(), EventLogError> {
        self.io.truncate(length)
    }
}

#[test]
fn mid_page_read_failure_or_final_length_change_cannot_return_a_partial_prefix() {
    for fail_read in [false, true] {
        let io = ChangingReadIo {
            io: MemoryIo::empty(),
            change_on_read: Rc::new(std::cell::Cell::new(None)),
            reads: Rc::new(std::cell::Cell::new(0)),
            fail_read,
        };
        let mut log =
            LogInner::open(io.clone(), binding(), key(), limits(), Machine::default()).unwrap();
        log.append(&event(1)).unwrap();
        log.append(&event(2)).unwrap();
        let cursor = log.start_cursor().unwrap();
        io.reads.set(0);
        // Header, first frame, then second frame. One valid event was already
        // accumulated when this deterministic read fault becomes visible.
        io.change_on_read.set(Some(3));
        assert!(matches!(
            log.read_page(&cursor, ordinary_limits()),
            Err(EventLogError::Changed)
        ));
        assert!(matches!(log.machine(), Err(EventLogError::Poisoned)));
        assert_eq!(io.io.0.borrow().truncations, 0);
    }
}

#[test]
fn pages_are_exact_ordered_retryable_and_can_continue_after_new_appends() {
    let io = MemoryIo::empty();
    let mut log = open(io.clone()).unwrap();
    let start = log.start_cursor().unwrap();
    let empty = log.read_page(&start, ordinary_limits()).unwrap();
    assert!(empty.at_end());
    assert!(empty.events().is_empty());
    for n in 1..=5 {
        log.append(&event(n)).unwrap();
    }
    let disk_before = io.bytes();
    let counters_before = {
        let disk = io.0.borrow();
        (disk.writes, disk.truncations)
    };
    let mut cursor = empty.next_cursor().clone();
    let mut records = Vec::new();
    loop {
        let page = log.read_page(&cursor, ordinary_limits()).unwrap();
        let retry = log.read_page(&cursor, ordinary_limits()).unwrap();
        assert_eq!(page.at_end(), retry.at_end());
        assert!(page.events().len() <= 2);
        assert_eq!(
            page.events()
                .iter()
                .map(CommittedEvent::event_bytes)
                .collect::<Vec<_>>(),
            retry
                .events()
                .iter()
                .map(CommittedEvent::event_bytes)
                .collect::<Vec<_>>()
        );
        let (events, next, at_end) = page.into_parts();
        records.extend(
            events
                .into_iter()
                .map(|e| (e.ordinal(), e.event_bytes().to_vec())),
        );
        cursor = next;
        if at_end {
            break;
        }
    }
    assert_eq!(
        records,
        (1_u8..=5)
            .map(|n| (u64::from(n), event(n)))
            .collect::<Vec<_>>()
    );
    assert_eq!(io.bytes(), disk_before);
    let disk = io.0.borrow();
    assert_eq!((disk.writes, disk.truncations), counters_before);
    assert!(disk.largest_read <= MAX_ENCODED_LOG_FRAME_BYTES as u64);
    drop(disk);
    assert!(
        log.read_page(&cursor, ordinary_limits())
            .unwrap()
            .events()
            .is_empty()
    );
    log.append(&event(6)).unwrap();
    let appended = log.read_page(&cursor, ordinary_limits()).unwrap();
    assert!(appended.at_end());
    assert_eq!(appended.events().len(), 1);
    assert_eq!(appended.events()[0].ordinal(), 6);
    assert_eq!(appended.events()[0].event_bytes(), event(6));
    assert_eq!(log.records, 6);
    assert_eq!(log.machine().unwrap().records.len(), 6);
}

#[test]
fn plaintext_budget_is_exact_and_every_legal_page_makes_progress() {
    for length in [MAX_LOG_PLAINTEXT_BYTES, MAX_LOG_PLAINTEXT_BYTES / 2 + 1] {
        let io = MemoryIo::empty();
        let mut log = open(io).unwrap();
        for n in 1..=3_u8 {
            let mut bytes = vec![n; length];
            bytes[0] = n;
            log.append(&bytes).unwrap();
        }
        let mut cursor = log.start_cursor().unwrap();
        for n in 1..=3 {
            let page = log.read_page(&cursor, ordinary_limits()).unwrap();
            assert_eq!(page.events().len(), 1);
            assert_eq!(page.events()[0].ordinal(), n);
            assert_eq!(page.plaintext_bytes(), length);
            assert_eq!(page.at_end(), n == 3);
            cursor = page.next_cursor().clone();
        }
    }
    let mut log = open(MemoryIo::empty()).unwrap();
    for n in 1..=3_u8 {
        log.append(&vec![n; MAX_LOG_PLAINTEXT_BYTES / 2]).unwrap();
    }
    let page = log
        .read_page(&log.start_cursor().unwrap(), ordinary_limits())
        .unwrap();
    assert_eq!(page.events().len(), 2);
    assert_eq!(page.plaintext_bytes(), MAX_LOG_PLAINTEXT_BYTES - 1);
    assert!(!page.at_end());
}

#[test]
fn invalid_limits_and_foreign_or_reopened_cursors_do_not_poison() {
    let io = MemoryIo::empty();
    let mut log = open(io.clone()).unwrap();
    log.append(&event(1)).unwrap();
    let cursor = log.start_cursor().unwrap();
    let before = io.bytes();
    for limits in [
        page_limits(0, MAX_LOG_PLAINTEXT_BYTES),
        page_limits(MAX_EVENT_PAGE_RECORDS + 1, MAX_LOG_PLAINTEXT_BYTES),
        page_limits(1, 0),
        page_limits(1, MAX_LOG_PLAINTEXT_BYTES - 1),
        page_limits(1, MAX_EVENT_PAGE_BYTES + 1),
        page_limits(usize::MAX, usize::MAX),
    ] {
        assert!(matches!(
            log.read_page(&cursor, limits),
            Err(EventLogError::InvalidPageLimits)
        ));
        assert!(log.machine().is_ok());
        assert_eq!(io.bytes(), before);
    }
    let mut different = open(MemoryIo::empty()).unwrap();
    assert!(matches!(
        different.read_page(&cursor, ordinary_limits()),
        Err(EventLogError::InvalidCursor)
    ));
    assert!(different.machine().is_ok());
    drop(log);
    let mut reopened = open(io).unwrap();
    assert!(matches!(
        reopened.read_page(&cursor, ordinary_limits()),
        Err(EventLogError::InvalidCursor)
    ));
    let fresh = reopened.start_cursor().unwrap();
    assert_eq!(
        reopened
            .read_page(&fresh, ordinary_limits())
            .unwrap()
            .events()[0]
            .event_bytes(),
        event(1)
    );
}

#[test]
fn apply_logs_never_expose_cursor_or_plaintext_pages() {
    let apply_binding = LogBinding::new(
        binding().folder_id(),
        binding().installation_id(),
        binding().generation_id(),
        LogFileKind::Apply,
    );
    let mut apply = LogInner::open(
        MemoryIo::empty(),
        apply_binding,
        key(),
        limits(),
        Machine::default(),
    )
    .unwrap();
    apply.append(&event(1)).unwrap();
    assert!(matches!(
        apply.start_cursor(),
        Err(EventLogError::WrongPageKind)
    ));
    let events = open(MemoryIo::empty()).unwrap();
    let cursor = events.start_cursor().unwrap();
    assert!(matches!(
        apply.read_page(&cursor, ordinary_limits()),
        Err(EventLogError::WrongPageKind)
    ));
    assert!(apply.machine().is_ok());
}

#[test]
fn post_open_corruption_or_external_growth_returns_no_partial_page_and_poisons() {
    for change in 0..5 {
        let io = MemoryIo::empty();
        let mut log = open(io.clone()).unwrap();
        log.append(&event(1)).unwrap();
        log.append(&event(2)).unwrap();
        let cursor = log.start_cursor().unwrap();
        let second_start = 8 + frame(1, &event(1)).len();
        {
            let mut disk = io.0.borrow_mut();
            match change {
                0 => disk.bytes.push(0),
                1 => {
                    disk.bytes.pop();
                }
                2 => disk.bytes[0] ^= 1,
                3 => disk.bytes[second_start + 30] ^= 1,
                _ => {
                    // A valid local AEAD frame with the right ordinal still
                    // must match accepted history, not merely authenticate.
                    let mut alternate = event(2);
                    *alternate.last_mut().unwrap() ^= 1;
                    disk.bytes[second_start..].copy_from_slice(&frame(2, &alternate));
                }
            }
        }
        let changed = io.bytes();
        assert!(log.read_page(&cursor, ordinary_limits()).is_err());
        assert!(matches!(log.machine(), Err(EventLogError::Poisoned)));
        assert!(matches!(log.start_cursor(), Err(EventLogError::Poisoned)));
        assert!(matches!(
            log.append(&event(1)),
            Err(EventLogError::Poisoned)
        ));
        assert_eq!(io.bytes(), changed);
        assert_eq!(io.0.borrow().truncations, 0);
    }
}

#[test]
fn uncertain_append_cannot_be_exported_until_successful_reopen() {
    let io = MemoryIo::empty();
    let mut log = open(io.clone()).unwrap();
    let cursor = log.start_cursor().unwrap();
    io.0.borrow_mut().fail_sync = true;
    assert!(log.append(&event(1)).is_err());
    assert!(matches!(
        log.read_page(&cursor, ordinary_limits()),
        Err(EventLogError::Poisoned)
    ));
    io.0.borrow_mut().fail_sync = false;
    let mut log = open(io).unwrap();
    assert!(matches!(
        log.read_page(&cursor, ordinary_limits()),
        Err(EventLogError::InvalidCursor)
    ));
    let page = log
        .read_page(&log.start_cursor().unwrap(), ordinary_limits())
        .unwrap();
    assert_eq!(page.events()[0].event_bytes(), event(1));
    assert!(page.at_end());
}

#[test]
fn real_private_file_pages_revalidate_lock_and_redact_plaintext() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let directory = PrivateStateDir::open_root(temp.path()).unwrap();
    let name = StateKey::new("events").unwrap();
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
    let cursor = log.start_cursor().unwrap();
    let page = log.read_page(&cursor, ordinary_limits()).unwrap();
    assert_eq!(page.events()[0].event_bytes(), event(1));
    let debug = format!("{cursor:?} {page:?} {:?}", page.events()[0]);
    assert!(!debug.contains("canary"));
    assert!(!debug.contains(temp.path().to_str().unwrap()));
    let old_lock = temp.path().join("retired-lock");
    fs::rename(temp.path().join("writer.lock"), old_lock).unwrap();
    fs::write(temp.path().join("writer.lock"), b"").unwrap();
    fs::set_permissions(
        temp.path().join("writer.lock"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(log.read_page(&cursor, ordinary_limits()).is_err());
    assert!(matches!(log.machine(), Err(EventLogError::Poisoned)));
}
