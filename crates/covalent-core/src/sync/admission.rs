//! Non-mutating causal-history admission checks for a future folder protocol.
//!
//! Call this only after an external wire layer authenticates the header and
//! authorizes its folder membership and epoch. This module does not sign or
//! persist anything, allocate counters, apply files, or replace the future
//! folder transaction that must recheck every result before appending.

use covalent_protocol::DeviceId;
use thiserror::Error;

use super::{VersionVector, VersionVectorOrder, register::OpId};

/// A fixed-size commitment digest supplied by an authenticated wire layer.
pub type Digest = [u8; 32];

/// The metadata needed to prove causal admission without retaining a body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    id: OpId,
    clock: VersionVector,
    digest: Digest,
    predecessor: Option<Digest>,
}

impl Header {
    /// Builds a header whose shape is checked by [`check`].
    ///
    /// Keeping construction separate lets the checker reject malformed remote
    /// headers rather than assuming a decoder already made them well-formed.
    #[must_use]
    pub fn new(
        id: OpId,
        clock: VersionVector,
        digest: Digest,
        predecessor: Option<Digest>,
    ) -> Self {
        Self {
            id,
            clock,
            digest,
            predecessor,
        }
    }

    /// Returns the folder-global author dot.
    #[must_use]
    pub const fn id(&self) -> OpId {
        self.id
    }

    /// Returns the full causal clock committed by this operation.
    #[must_use]
    pub const fn clock(&self) -> &VersionVector {
        &self.clock
    }

    /// Returns the authenticated commitment digest.
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Returns the immediately preceding same-author commitment, if any.
    #[must_use]
    pub const fn predecessor(&self) -> Option<Digest> {
        self.predecessor
    }
}

/// The durable, contiguous head of one author's folder-wide sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorTip {
    counter: u64,
    digest: Digest,
}

impl AuthorTip {
    /// Creates a nonzero contiguous author tip.
    pub fn new(counter: u64, digest: Digest) -> Result<Self, AdmissionError> {
        if counter == 0 {
            return Err(AdmissionError::InvalidAuthorTip);
        }
        Ok(Self { counter, digest })
    }

    /// Returns the durable contiguous author counter.
    #[must_use]
    pub const fn counter(self) -> u64 {
        self.counter
    }

    /// Returns the commitment at that counter.
    #[must_use]
    pub const fn digest(self) -> Digest {
        self.digest
    }
}

/// A bounded read failure from a future disk-backed history query.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HistoryQueryError {
    /// The lookup could not complete without making an admission decision.
    #[error("causal history lookup is unavailable")]
    Unavailable,
}

/// Read-only exact-header and contiguous-tip queries for one folder log.
///
/// A production implementation must expose only exact headers that already
/// passed causal admission for this same folder and came from committed or
/// index-replayed contiguous history. Authenticated raw disk headers are not
/// sufficient. It must also bound disk work. The checker never requests bodies
/// or buffers a history; it looks up at most one header for every actor in a
/// 128-actor vector plus the candidate dot.
pub trait History {
    /// Returns the committed or index-replayed contiguous tip for `writer`, or
    /// no tip at counter 0.
    fn author_tip(&self, writer: DeviceId) -> Result<Option<AuthorTip>, HistoryQueryError>;

    /// Returns the exact historical header at `id`, or absence.
    fn header(&self, id: OpId) -> Result<Option<Header>, HistoryQueryError>;
}

/// A successful non-mutating admission decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Admission {
    /// The exact commitment already exists and must not be applied again.
    Duplicate,
    /// The candidate is ready for a future locked append transaction.
    Admissible,
}

/// A candidate or retained-history condition that prevents admission.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AdmissionError {
    /// A nonempty durable author tip must have a positive counter.
    #[error("history returned an invalid zero author tip")]
    InvalidAuthorTip,
    /// The candidate's author component differs from its dot counter.
    #[error("candidate dot does not match its full causal clock")]
    CandidateClockDoesNotMatchDot,
    /// The same historical dot has a different retained commitment.
    #[error("candidate conflicts with an existing historical dot")]
    Equivocation,
    /// Advancing the retained tip cannot represent another author operation.
    #[error("author counter cannot advance past u64::MAX")]
    AuthorCounterOverflow,
    /// The candidate skips one or more author operations after the known tip.
    #[error("candidate author counter skips a required predecessor")]
    MissingAuthorPredecessor,
    /// A candidate at or below the tip has no retained exact historical header.
    #[error("history is missing an exact header below its contiguous author tip")]
    CorruptHistoryBelowTip,
    /// The candidate does not link to the retained same-author tip digest.
    #[error("candidate predecessor digest does not match the author tip")]
    WrongPredecessor,
    /// The retained author tip disagrees with its exact prior header.
    #[error("author tip digest does not match its exact historical header")]
    CorruptAuthorTip,
    /// An exact header required by the candidate's causal context is absent.
    #[error("candidate causal dependency is unavailable")]
    UnknownDependency,
    /// A history query returned a header for a different requested dot.
    #[error("history returned a header for a different dot")]
    DependencyDotMismatch,
    /// A retained dependency's author component does not match its own dot.
    #[error("retained dependency dot does not match its full causal clock")]
    CorruptDependencyClock,
    /// A dependency proves the candidate omitted part of its causal context.
    #[error("candidate causal context is not closed over a dependency")]
    CausalClosureViolation,
    /// The read-only history backend could not complete its bounded lookup.
    #[error(transparent)]
    HistoryQuery(#[from] HistoryQueryError),
}

/// Checks a candidate against an immutable folder history without appending it.
///
/// The result is only a preflight. A later durable folder transaction must
/// recheck dot uniqueness, tip, authorization, and resource limits before it
/// appends or acknowledges the operation.
pub fn check(history: &impl History, candidate: &Header) -> Result<Admission, AdmissionError> {
    let author = candidate.id.actor();
    let counter = candidate.id.counter();
    if candidate.clock.counter(author) != counter {
        return Err(AdmissionError::CandidateClockDoesNotMatchDot);
    }

    if let Some(existing) = history.header(candidate.id)? {
        return if existing == *candidate {
            Ok(Admission::Duplicate)
        } else {
            Err(AdmissionError::Equivocation)
        };
    }

    let tip = history.author_tip(author)?;
    match tip {
        None if counter == 1 && candidate.predecessor.is_none() => {}
        None if counter == 1 => return Err(AdmissionError::WrongPredecessor),
        None => return Err(AdmissionError::MissingAuthorPredecessor),
        Some(tip) => {
            let next = tip
                .counter
                .checked_add(1)
                .ok_or(AdmissionError::AuthorCounterOverflow)?;
            if counter <= tip.counter {
                return Err(AdmissionError::CorruptHistoryBelowTip);
            }
            if counter != next {
                return Err(AdmissionError::MissingAuthorPredecessor);
            }
            if candidate.predecessor != Some(tip.digest) {
                return Err(AdmissionError::WrongPredecessor);
            }
        }
    }

    let mut context = candidate.clock.counters.clone();
    if counter == 1 {
        context.remove(&author);
    } else {
        context.insert(author, counter - 1);
    }
    let context = VersionVector { counters: context };
    for (dependency_author, dependency_counter) in &context.counters {
        let dependency_id = OpId::new(*dependency_author, *dependency_counter)
            .expect("version vectors contain only positive counters");
        let dependency = history
            .header(dependency_id)?
            .ok_or(AdmissionError::UnknownDependency)?;
        if dependency.id != dependency_id {
            return Err(AdmissionError::DependencyDotMismatch);
        }
        if dependency_author == &author
            && *dependency_counter + 1 == counter
            && tip.is_some_and(|author_tip| dependency.digest != author_tip.digest)
        {
            return Err(AdmissionError::CorruptAuthorTip);
        }
        if dependency.clock.counter(*dependency_author) != *dependency_counter {
            return Err(AdmissionError::CorruptDependencyClock);
        }
        if !matches!(
            context.compare(&dependency.clock),
            VersionVectorOrder::Equal | VersionVectorOrder::After
        ) {
            return Err(AdmissionError::CausalClosureViolation);
        }
    }

    Ok(Admission::Admissible)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct MemoryHistory {
        headers: BTreeMap<OpId, Header>,
        tips: BTreeMap<DeviceId, AuthorTip>,
        unavailable: bool,
        wrong_request: Option<OpId>,
        wrong_header: Option<Header>,
    }

    impl History for MemoryHistory {
        fn author_tip(&self, writer: DeviceId) -> Result<Option<AuthorTip>, HistoryQueryError> {
            if self.unavailable {
                return Err(HistoryQueryError::Unavailable);
            }
            Ok(self.tips.get(&writer).copied())
        }

        fn header(&self, id: OpId) -> Result<Option<Header>, HistoryQueryError> {
            if self.unavailable {
                return Err(HistoryQueryError::Unavailable);
            }
            if self.wrong_request == Some(id) {
                return Ok(self.wrong_header.clone());
            }
            Ok(self.headers.get(&id).cloned())
        }
    }

    fn actor(number: u128) -> DeviceId {
        DeviceId::from_uuid(uuid::Uuid::from_u128(number + 1))
    }

    fn digest(number: u8) -> Digest {
        [number; 32]
    }

    fn header(
        writer: u128,
        counter: u64,
        clock: &[(u128, u64)],
        digest_number: u8,
        predecessor: Option<u8>,
    ) -> Header {
        Header::new(
            OpId::new(actor(writer), counter).expect("positive test dot"),
            VersionVector::new(
                clock
                    .iter()
                    .map(|(writer, counter)| (actor(*writer), *counter)),
            )
            .expect("bounded test clock"),
            digest(digest_number),
            predecessor.map(digest),
        )
    }

    fn retain(history: &mut MemoryHistory, header: Header) {
        let id = header.id();
        history.tips.insert(
            id.actor(),
            AuthorTip::new(id.counter(), header.digest()).expect("positive tip"),
        );
        history.headers.insert(id, header);
    }

    #[test]
    fn valid_first_and_contiguous_operations_are_admissible() {
        let mut history = MemoryHistory::default();
        let first = header(0, 1, &[(0, 1)], 1, None);
        assert_eq!(check(&history, &first), Ok(Admission::Admissible));
        retain(&mut history, first);
        let second = header(0, 2, &[(0, 2)], 2, Some(1));
        assert_eq!(check(&history, &second), Ok(Admission::Admissible));
    }

    #[test]
    fn exact_duplicate_and_dominated_historical_equivocation_are_distinct() {
        let mut history = MemoryHistory::default();
        let first = header(0, 1, &[(0, 1)], 1, None);
        retain(&mut history, first.clone());
        let later = header(0, 2, &[(0, 2)], 2, Some(1));
        retain(&mut history, later);

        assert_eq!(check(&history, &first), Ok(Admission::Duplicate));
        let conflict = header(0, 1, &[(0, 1)], 9, None);
        assert_eq!(
            check(&history, &conflict),
            Err(AdmissionError::Equivocation)
        );
    }

    #[test]
    fn skipped_and_missing_historical_author_counters_are_rejected() {
        let mut history = MemoryHistory::default();
        let first = header(0, 1, &[(0, 1)], 1, None);
        let second = header(0, 2, &[(0, 2)], 2, Some(1));
        retain(&mut history, first);
        retain(&mut history, second.clone());
        let skipped = header(0, 4, &[(0, 4)], 4, Some(2));
        assert_eq!(
            check(&history, &skipped),
            Err(AdmissionError::MissingAuthorPredecessor)
        );

        history.headers.remove(&second.id());
        let missing = header(0, 2, &[(0, 2)], 2, Some(1));
        assert_eq!(
            check(&history, &missing),
            Err(AdmissionError::CorruptHistoryBelowTip)
        );
    }

    #[test]
    fn predecessor_and_author_clock_must_match_retained_history() {
        let mut history = MemoryHistory::default();
        let first = header(0, 1, &[(0, 1)], 1, None);
        retain(&mut history, first);
        let wrong_predecessor = header(0, 2, &[(0, 2)], 2, Some(9));
        assert_eq!(
            check(&history, &wrong_predecessor),
            Err(AdmissionError::WrongPredecessor)
        );
        let malformed = header(0, 2, &[(0, 1)], 2, Some(1));
        assert_eq!(
            check(&history, &malformed),
            Err(AdmissionError::CandidateClockDoesNotMatchDot)
        );
    }

    #[test]
    fn out_of_order_dependency_waits_then_becomes_admissible() {
        let mut history = MemoryHistory::default();
        let dependent = header(1, 1, &[(1, 1), (2, 1)], 2, None);
        assert_eq!(
            check(&history, &dependent),
            Err(AdmissionError::UnknownDependency)
        );
        let dependency = header(2, 1, &[(2, 1)], 1, None);
        retain(&mut history, dependency);
        assert_eq!(check(&history, &dependent), Ok(Admission::Admissible));
    }

    #[test]
    fn full_context_rejects_omitted_transitive_dependency_and_writer_history() {
        let mut history = MemoryHistory::default();
        let c1 = header(2, 1, &[(2, 1)], 1, None);
        let b1 = header(1, 1, &[(1, 1), (2, 1)], 2, None);
        retain(&mut history, c1);
        retain(&mut history, b1);
        let forged_a1 = header(0, 1, &[(0, 1), (1, 1)], 3, None);
        assert_eq!(
            check(&history, &forged_a1),
            Err(AdmissionError::CausalClosureViolation)
        );

        let a1 = header(0, 1, &[(0, 1), (1, 1), (2, 1)], 4, None);
        retain(&mut history, a1);
        let forged_a2 = header(0, 2, &[(0, 2)], 5, Some(4));
        assert_eq!(
            check(&history, &forged_a2),
            Err(AdmissionError::CausalClosureViolation)
        );
    }

    #[test]
    fn corrupted_dependency_and_lookup_failure_are_not_admitted() {
        let mut history = MemoryHistory::default();
        let malformed = header(1, 1, &[(1, 2)], 1, None);
        history.headers.insert(malformed.id(), malformed);
        let candidate = header(0, 1, &[(0, 1), (1, 1)], 2, None);
        assert_eq!(
            check(&history, &candidate),
            Err(AdmissionError::CorruptDependencyClock)
        );

        history.unavailable = true;
        assert_eq!(
            check(&history, &candidate),
            Err(AdmissionError::HistoryQuery(HistoryQueryError::Unavailable))
        );
    }

    #[test]
    fn author_tip_must_match_the_exact_prior_header_and_dots_reject_zero() {
        assert!(OpId::new(actor(0), 0).is_err());
        let mut history = MemoryHistory::default();
        let first = header(0, 1, &[(0, 1)], 1, None);
        history.headers.insert(first.id(), first);
        history.tips.insert(
            actor(0),
            AuthorTip::new(1, digest(9)).expect("positive tip"),
        );
        let candidate = header(0, 2, &[(0, 2)], 2, Some(9));
        assert_eq!(
            check(&history, &candidate),
            Err(AdmissionError::CorruptAuthorTip)
        );
    }

    #[test]
    fn first_counter_cannot_claim_a_predecessor_and_author_overflow_rejects() {
        let unexpected_predecessor = header(0, 1, &[(0, 1)], 1, Some(9));
        assert_eq!(
            check(&MemoryHistory::default(), &unexpected_predecessor),
            Err(AdmissionError::WrongPredecessor)
        );

        let mut history = MemoryHistory::default();
        history.tips.insert(
            actor(0),
            AuthorTip::new(u64::MAX, digest(9)).expect("maximum tip"),
        );
        let maximum = header(0, u64::MAX, &[(0, u64::MAX)], 10, Some(9));
        assert_eq!(
            check(&history, &maximum),
            Err(AdmissionError::AuthorCounterOverflow)
        );
    }

    #[test]
    fn history_must_return_the_exact_requested_dependency_header() {
        let requested = OpId::new(actor(1), 1).expect("requested dependency");
        let mut history = MemoryHistory {
            wrong_request: Some(requested),
            wrong_header: Some(header(2, 1, &[(2, 1)], 1, None)),
            ..MemoryHistory::default()
        };
        let candidate = header(0, 1, &[(0, 1), (1, 1)], 2, None);
        assert_eq!(
            check(&history, &candidate),
            Err(AdmissionError::DependencyDotMismatch)
        );
        history.wrong_request = None;
        assert_eq!(
            check(&history, &candidate),
            Err(AdmissionError::UnknownDependency)
        );
    }
}
