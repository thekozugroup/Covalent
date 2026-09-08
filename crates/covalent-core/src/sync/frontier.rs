//! Bounded causal-closure checks against an already admitted folder history.
//!
//! A frontier names exact historical operations, rather than merely counters
//! below a current tip. This check is useful before accepting bootstrap or
//! write-loss evidence. It does not authenticate the history, prove a complete
//! inventory, or replace the lock and durable transaction around acceptance.

use thiserror::Error;

use super::admission::{History, HistoryQueryError};
use super::register::OpId;
use super::{VersionVector, VersionVectorOrder};

/// A claimed frontier cannot yet be used as causally closed evidence.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FrontierError {
    #[error("frontier history lookup is unavailable")]
    HistoryUnavailable,
    #[error("frontier requires a missing historical operation")]
    MissingOperation,
    #[error("frontier history returned a different operation")]
    WrongOperation,
    #[error("frontier historical operation has an inconsistent author clock")]
    InconsistentClock,
    #[error("frontier omits a historical operation's causal dependency")]
    NotClosed,
}

impl From<HistoryQueryError> for FrontierError {
    fn from(_: HistoryQueryError) -> Self {
        Self::HistoryUnavailable
    }
}

/// Checks every nonzero component against its exact admitted historical header.
///
/// The supplied history must meet [`History`]'s same-folder, authenticated,
/// contiguous admission contract. At most 128 exact headers are queried, one
/// at a time; bodies and unrelated history are never read. A historical closed
/// frontier may be behind current author tips. An empty frontier is closed.
/// Success alone grants no permission and proves no physical file application.
pub fn check_closed_frontier(
    history: &impl History,
    frontier: &VersionVector,
) -> Result<(), FrontierError> {
    for (&writer, &counter) in &frontier.counters {
        let id = OpId::new(writer, counter).expect("version-vector counters are positive");
        let header = history.header(id)?.ok_or(FrontierError::MissingOperation)?;
        if header.id() != id {
            return Err(FrontierError::WrongOperation);
        }
        if header.clock().counter(writer) != counter {
            return Err(FrontierError::InconsistentClock);
        }
        if !matches!(
            frontier.compare(header.clock()),
            VersionVectorOrder::Equal | VersionVectorOrder::After
        ) {
            return Err(FrontierError::NotClosed);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use covalent_protocol::DeviceId;

    use super::*;
    use crate::sync::admission::{self, Admission, AuthorTip, Header};

    #[derive(Default)]
    struct TestHistory {
        headers: BTreeMap<OpId, Header>,
        tips: BTreeMap<DeviceId, AuthorTip>,
        queries: Cell<usize>,
        unavailable: bool,
        substituted: Option<Header>,
    }

    impl History for TestHistory {
        fn author_tip(&self, writer: DeviceId) -> Result<Option<AuthorTip>, HistoryQueryError> {
            Ok(self.tips.get(&writer).copied())
        }

        fn header(&self, id: OpId) -> Result<Option<Header>, HistoryQueryError> {
            self.queries.set(self.queries.get() + 1);
            if self.unavailable {
                return Err(HistoryQueryError::Unavailable);
            }
            Ok(self
                .substituted
                .clone()
                .or_else(|| self.headers.get(&id).cloned()))
        }
    }

    fn writer(n: u128) -> DeviceId {
        DeviceId::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn vector(entries: &[(u128, u64)]) -> VersionVector {
        VersionVector::new(entries.iter().map(|&(n, counter)| (writer(n), counter)))
            .expect("test vector")
    }

    fn append(history: &mut TestHistory, n: u128, dependencies: &[(u128, u64)]) -> Header {
        let author = writer(n);
        let previous = history.tips.get(&author).copied();
        let counter = previous.map_or(1, |tip| tip.counter() + 1);
        let (clock, _) = vector(dependencies)
            .checked_advance(author)
            .expect("test advance");
        assert_eq!(clock.counter(author), counter);
        let header = Header::new(
            OpId::new(author, counter).expect("test dot"),
            clock,
            *blake3::hash(format!("{n}:{counter}").as_bytes()).as_bytes(),
            previous.map(AuthorTip::digest),
        );
        assert_eq!(
            admission::check(history, &header),
            Ok(Admission::Admissible)
        );
        history
            .tips
            .insert(author, AuthorTip::new(counter, header.digest()).unwrap());
        history.headers.insert(header.id(), header.clone());
        header
    }

    #[test]
    fn empty_frontier_does_not_read_history() {
        let history = TestHistory {
            unavailable: true,
            ..TestHistory::default()
        };
        assert_eq!(
            check_closed_frontier(&history, &VersionVector::empty()),
            Ok(())
        );
        assert_eq!(history.queries.get(), 0);
    }

    #[test]
    fn exact_historical_frontier_is_valid_even_behind_current_tips() {
        let mut history = TestHistory::default();
        append(&mut history, 1, &[]);
        append(&mut history, 2, &[(1, 1)]);
        append(&mut history, 1, &[(1, 1), (2, 1)]);
        history.queries.set(0);
        assert_eq!(
            check_closed_frontier(&history, &vector(&[(1, 1), (2, 1)])),
            Ok(())
        );
        assert_eq!(history.queries.get(), 2);
    }

    #[test]
    fn transitive_dependency_may_not_be_omitted_or_downgraded() {
        let mut history = TestHistory::default();
        append(&mut history, 3, &[]);
        append(&mut history, 3, &[(3, 1)]);
        append(&mut history, 2, &[(3, 2)]);
        append(&mut history, 1, &[(2, 1), (3, 2)]);
        for bad in [vector(&[(1, 1), (2, 1)]), vector(&[(1, 1), (2, 1), (3, 1)])] {
            assert_eq!(
                check_closed_frontier(&history, &bad),
                Err(FrontierError::NotClosed)
            );
        }
        assert_eq!(
            check_closed_frontier(&history, &vector(&[(1, 1), (2, 1), (3, 2)])),
            Ok(())
        );
    }

    #[test]
    fn missing_and_unavailable_history_fail_closed() {
        let mut history = TestHistory::default();
        assert_eq!(
            check_closed_frontier(&history, &vector(&[(1, 1)])),
            Err(FrontierError::MissingOperation)
        );
        history.unavailable = true;
        assert_eq!(
            check_closed_frontier(&history, &vector(&[(1, 1)])),
            Err(FrontierError::HistoryUnavailable)
        );
    }

    #[test]
    fn inconsistent_history_is_rejected() {
        let mut history = TestHistory::default();
        let first = append(&mut history, 1, &[]);
        history.substituted = Some(first.clone());
        assert_eq!(
            check_closed_frontier(&history, &vector(&[(2, 1)])),
            Err(FrontierError::WrongOperation)
        );
        history.substituted = Some(Header::new(
            first.id(),
            vector(&[(1, 2)]),
            first.digest(),
            None,
        ));
        assert_eq!(
            check_closed_frontier(&history, &vector(&[(1, 1)])),
            Err(FrontierError::InconsistentClock)
        );
    }

    #[test]
    fn maximum_frontier_uses_exactly_one_bounded_query_per_writer() {
        let mut history = TestHistory::default();
        for n in 1..=128 {
            append(&mut history, n, &[]);
        }
        let frontier = vector(&(1..=128).map(|n| (n, 1)).collect::<Vec<_>>());
        history.queries.set(0);
        assert_eq!(check_closed_frontier(&history, &frontier), Ok(()));
        assert_eq!(history.queries.get(), 128);
    }
}
