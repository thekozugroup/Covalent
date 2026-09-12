use std::cell::Cell;

use super::*;
use crate::JobControl;

fn interrupted_at_every_check(bytes: Vec<u8>, expected_prefix: &[u8]) {
    let observed = Cell::new(0);
    let probe = MemoryIo::from_bytes(bytes.clone());
    let successful = LogInner::open_checked(
        probe,
        binding(),
        key(),
        limits(),
        Machine::default(),
        &mut || {
            observed.set(observed.get() + 1);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(successful.records, 3);
    for stop_at in 1..=observed.get() {
        let io = MemoryIo::from_bytes(bytes.clone());
        let mut visits = 0;
        let result = LogInner::open_checked(
            io.clone(),
            binding(),
            key(),
            limits(),
            Machine::default(),
            &mut || {
                visits += 1;
                if visits == stop_at {
                    Err(EventLogError::Interrupted)
                } else {
                    Ok(())
                }
            },
        );
        assert!(
            matches!(result, Err(EventLogError::Interrupted)),
            "check {stop_at}"
        );
        let disk = io.0.borrow();
        assert!(disk.bytes == bytes || disk.bytes == expected_prefix);
        // If cancellation is observed after a tail repair, its file sync must
        // already have completed. No partial machine or success is returned.
        if disk.truncations > 0 {
            assert_eq!(disk.bytes, expected_prefix);
            assert_eq!(disk.synced, expected_prefix);
        }
        assert_eq!(disk.writes, 0);
        if stop_at == 1 {
            assert_eq!(disk.largest_read, 0);
        }
        drop(disk);
        let replay = open(io).unwrap();
        assert_eq!(replay.records, 3);
    }
}

#[test]
fn cancellation_at_each_replay_boundary_exposes_no_partial_state_or_unstable_repair() {
    let mut prefix = SYNC_LOG_FILE_MAGIC.to_vec();
    for n in 1..=3 {
        prefix.extend_from_slice(&frame(n, &event(n as u8)));
    }
    interrupted_at_every_check(prefix.clone(), &prefix);
    let mut incomplete = prefix.clone();
    let fourth = frame(4, &event(4));
    incomplete.extend_from_slice(&fourth[..fourth.len() - 1]);
    interrupted_at_every_check(incomplete, &prefix);
}

#[test]
fn cancellation_does_not_turn_complete_corruption_into_tail_repair() {
    let mut bytes = SYNC_LOG_FILE_MAGIC.to_vec();
    bytes.extend_from_slice(&frame(1, &event(1)));
    let mut corrupt = frame(2, &event(2));
    corrupt[70] ^= 1;
    bytes.extend_from_slice(&corrupt);
    let io = MemoryIo::from_bytes(bytes.clone());
    let result = LogInner::open_checked(
        io.clone(),
        binding(),
        key(),
        limits(),
        Machine::default(),
        &mut || Ok(()),
    );
    assert!(result.is_err());
    assert_eq!(io.bytes(), bytes);
    assert_eq!(io.0.borrow().truncations, 0);
}

struct CancellingMachine {
    inner: Machine,
    control: JobControl,
}
impl EventMachine for CancellingMachine {
    type Prepared = Vec<u8>;
    fn is_empty_for_replay(&self) -> bool {
        self.inner.is_empty_for_replay()
    }
    fn prepare(&self, bytes: &[u8]) -> Result<PreparedEvent<Self::Prepared>, EventValidationError> {
        self.inner.prepare(bytes)
    }
    fn commit(&mut self, prepared: Self::Prepared) -> Result<(), EventValidationError> {
        self.inner.commit(prepared)?;
        self.control.cancel();
        Ok(())
    }
}

#[test]
fn public_controlled_open_releases_the_real_lock_after_mid_replay_cancel() {
    let temporary = tempfile::tempdir().unwrap();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let directory = PrivateStateDir::open_root(temporary.path()).unwrap();
    let name = StateKey::new("controlled.v1").unwrap();
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
    log.append(&event(2)).unwrap();
    drop(log);
    let original = fs::read(temporary.path().join("controlled.v1")).unwrap();
    let control = JobControl::new();
    let result = DurableEventLog::open_with_control(
        &directory,
        &name,
        binding(),
        key(),
        limits(),
        CancellingMachine {
            inner: Machine::default(),
            control: control.clone(),
        },
        &control,
    );
    assert!(matches!(result, Err(EventLogError::Interrupted)));
    assert_eq!(
        fs::read(temporary.path().join("controlled.v1")).unwrap(),
        original
    );
    let guard = directory.try_lock().unwrap();
    drop(guard);
    control.resume();
    let reopened = DurableEventLog::open_with_control(
        &directory,
        &name,
        binding(),
        key(),
        limits(),
        Machine::default(),
        &control,
    )
    .unwrap();
    assert_eq!(reopened.committed_records().unwrap(), 2);
}

#[test]
fn paused_open_stops_before_lock_creation_or_file_access() {
    let temporary = tempfile::tempdir().unwrap();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let directory = PrivateStateDir::open_root(temporary.path()).unwrap();
    let control = JobControl::new();
    control.pause();
    let result = DurableEventLog::open_with_control(
        &directory,
        &StateKey::new("missing.v1").unwrap(),
        binding(),
        key(),
        limits(),
        Machine::default(),
        &control,
    );
    assert!(matches!(result, Err(EventLogError::Interrupted)));
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
}
