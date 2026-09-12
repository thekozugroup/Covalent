//! Pure bounded multi-value causal-register arithmetic.
//!
//! This module accepts only already-decoded values. It does not authenticate a
//! value, prove that a clock's dependencies were observed, validate membership,
//! or retain historical equivocation evidence. Those are protocol and durable
//! operation-log responsibilities before a value reaches this math.
//! The 128-value cap bounds causal branches, not application-value bytes;
//! callers must bound decoded payloads before constructing entries.

use std::collections::BTreeMap;

use covalent_protocol::DeviceId;
use thiserror::Error;

use super::{MAX_VERSION_VECTOR_ACTORS, VersionVector, VersionVectorError, VersionVectorOrder};

/// A stable author dot within a folder-wide append-only operation sequence.
///
/// The counter belongs to the author's *folder-global* sequence. It must not
/// be allocated from a single path's register: an author can update many paths
/// and reusing a counter would make distinct operations indistinguishable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OpId {
    actor: DeviceId,
    counter: u64,
}

impl OpId {
    /// Creates one positive causal dot.
    pub fn new(actor: DeviceId, counter: u64) -> Result<Self, RegisterError> {
        if counter == 0 {
            return Err(RegisterError::ZeroDot);
        }
        Ok(Self { actor, counter })
    }

    /// Returns the author identity.
    #[must_use]
    pub const fn actor(self) -> DeviceId {
        self.actor
    }

    /// Returns the strictly positive author counter.
    #[must_use]
    pub const fn counter(self) -> u64 {
        self.counter
    }
}

/// An immutable causal value associated with one path by a future protocol.
///
/// `publish` is not authorization: its caller must supply the actual observed
/// folder frontier, including this author's last durable operation across all
/// paths. The constructor cannot prove that claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterEntry<T: Clone + Eq> {
    id: OpId,
    clock: VersionVector,
    value: T,
}

impl<T: Clone + Eq> RegisterEntry<T> {
    /// Builds an entry by advancing an observed folder-wide frontier.
    pub fn publish(
        observed_folder_frontier: &VersionVector,
        actor: DeviceId,
        value: T,
    ) -> Result<Self, RegisterError> {
        let (clock, counter) = observed_folder_frontier.checked_advance(actor)?;
        Ok(Self {
            id: OpId { actor, counter },
            clock,
            value,
        })
    }

    /// Reconstructs an already-decoded entry after checking its causal shape.
    ///
    /// Future wire handling must separately authenticate canonical bytes,
    /// membership, the author chain, and every dependency in `clock`.
    pub fn from_full_clock(
        id: OpId,
        clock: VersionVector,
        value: T,
    ) -> Result<Self, RegisterError> {
        if clock.counter(id.actor) != id.counter {
            return Err(RegisterError::DotDoesNotMatchClock);
        }
        Ok(Self { id, clock, value })
    }

    /// Returns the immutable author dot.
    #[must_use]
    pub const fn id(&self) -> OpId {
        self.id
    }

    /// Returns the full causal clock, not merely the author's context.
    #[must_use]
    pub const fn clock(&self) -> &VersionVector {
        &self.clock
    }

    /// Returns the application value retained by this concurrent branch.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }
}

/// A bounded antichain of causally active values for one future path.
///
/// Merge is a partial semilattice: it is associative, commutative, and
/// idempotent for valid histories whose resulting active antichain has at most
/// 128 values. A larger concurrent result is rejected rather than dropping a
/// value arbitrarily.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CausalRegister<T: Clone + Eq> {
    active: BTreeMap<OpId, RegisterEntry<T>>,
}

impl<T: Clone + Eq> Default for CausalRegister<T> {
    fn default() -> Self {
        Self {
            active: BTreeMap::new(),
        }
    }
}

impl<T: Clone + Eq> CausalRegister<T> {
    /// Returns an empty register.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns active values in canonical `OpId` order.
    pub fn active(&self) -> impl ExactSizeIterator<Item = &RegisterEntry<T>> {
        self.active.values()
    }

    /// Returns the number of concurrent active values.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Merges one entry without changing this register on any failure.
    pub fn merge_entry(&self, entry: RegisterEntry<T>) -> Result<Self, RegisterError> {
        let mut singleton = BTreeMap::new();
        singleton.insert(entry.id, entry);
        self.merge(&Self { active: singleton })
    }

    /// Computes the causal antichain for the union of both registers.
    ///
    /// The complete union is validated before dominance reduction. This avoids
    /// a sequential intermediate-capacity failure when a later value dominates
    /// enough entries for the final antichain to fit.
    pub fn merge(&self, other: &Self) -> Result<Self, RegisterError> {
        let mut union = self.active.clone();
        for (id, entry) in &other.active {
            if let Some(existing) = union.get(id) {
                if existing.clock != entry.clock || existing.value != entry.value {
                    return Err(RegisterError::OpIdMismatch);
                }
            } else {
                union.insert(*id, entry.clone());
            }
        }

        let entries: Vec<_> = union.values().collect();
        for (index, left) in entries.iter().enumerate() {
            for right in entries.iter().skip(index + 1) {
                if left.clock == right.clock && left.id != right.id {
                    return Err(RegisterError::EqualClockDifferentDot);
                }
            }
        }

        let mut active = BTreeMap::new();
        for candidate in &entries {
            let dominated = entries.iter().any(|other_entry| {
                other_entry.id != candidate.id
                    && other_entry.clock.compare(&candidate.clock) == VersionVectorOrder::After
            });
            if !dominated {
                active.insert(candidate.id, (*candidate).clone());
            }
        }
        if active.len() > MAX_VERSION_VECTOR_ACTORS {
            return Err(RegisterError::TooManyActiveValues);
        }
        Ok(Self { active })
    }
}

/// A rejected causal-register shape or bounded merge.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RegisterError {
    /// A dot with counter zero has no causal event identity.
    #[error("register operation counter must be positive")]
    ZeroDot,
    /// The full clock does not include the author's dot at the declared value.
    #[error("register operation dot does not match its full clock")]
    DotDoesNotMatchClock,
    /// The same dot carried a different full clock or application value.
    #[error("register operation id has conflicting content")]
    OpIdMismatch,
    /// Distinct dots claimed the same full causal clock.
    #[error("distinct register operations have equal full clocks")]
    EqualClockDifferentDot,
    /// More concurrent values would require arbitrary data loss.
    #[error("register has too many concurrent active values")]
    TooManyActiveValues,
    /// Underlying vector arithmetic rejected the causal shape.
    #[error(transparent)]
    VersionVector(#[from] VersionVectorError),
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum TestValue {
        File(&'static str),
        Directory,
        Tombstone,
    }

    #[derive(Clone)]
    struct HonestEvent {
        entry: RegisterEntry<TestValue>,
        clock: [u64; 3],
    }

    fn actor(number: u128) -> DeviceId {
        DeviceId::from_uuid(uuid::Uuid::from_u128(number + 1))
    }

    fn entry(
        frontier: &VersionVector,
        actor_id: u128,
        value: TestValue,
    ) -> RegisterEntry<TestValue> {
        RegisterEntry::publish(frontier, actor(actor_id), value).expect("valid entry")
    }

    fn value(kind: u8) -> TestValue {
        match kind % 3 {
            0 => TestValue::File("file"),
            1 => TestValue::Directory,
            _ => TestValue::Tombstone,
        }
    }

    fn vector(clock: [u64; 3]) -> VersionVector {
        VersionVector::new(
            clock
                .into_iter()
                .enumerate()
                .filter_map(|(index, counter)| {
                    (counter != 0).then_some((actor(index as u128), counter))
                }),
        )
        .expect("three-writer vector")
    }

    /// Simulates authenticated, dependency-complete operations before their
    /// independently shuffled delivery to this math primitive. It does not
    /// test protocol dependency enforcement.
    fn honest_history(script: &[(u8, u32, u8)]) -> Vec<HonestEvent> {
        let mut writer_frontiers = [[0_u64; 3]; 3];
        let mut history: Vec<HonestEvent> = Vec::with_capacity(script.len());
        for (writer, receives, kind) in script {
            let writer = usize::from(*writer % 3);
            let mut observed = writer_frontiers[writer];
            for (previous_index, previous) in history.iter().enumerate() {
                let bit = (receives.rotate_left((previous_index % 32) as u32) >> 31) & 1;
                if bit == 1 {
                    for (current, received) in observed.iter_mut().zip(previous.clock) {
                        *current = (*current).max(received);
                    }
                }
            }
            let mut clock = observed;
            clock[writer] = clock[writer]
                .checked_add(1)
                .expect("at most 24 honest events");
            let entry =
                RegisterEntry::publish(&vector(observed), actor(writer as u128), value(*kind))
                    .expect("bounded honest publication");
            writer_frontiers[writer] = clock;
            history.push(HonestEvent { entry, clock });
        }
        history
    }

    fn shuffled_indices(length: usize, mut seed: u32) -> Vec<usize> {
        let mut result: Vec<_> = (0..length).collect();
        for index in (1..length).rev() {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            result.swap(index, (seed as usize) % (index + 1));
        }
        result
    }

    fn deliver(
        initial: CausalRegister<TestValue>,
        history: &[HonestEvent],
        indices: impl IntoIterator<Item = usize>,
        seed: u32,
    ) -> CausalRegister<TestValue> {
        indices
            .into_iter()
            .enumerate()
            .try_fold(initial, |current, (position, index)| {
                let merged = current.merge_entry(history[index].entry.clone())?;
                if (seed.rotate_left((position % 32) as u32) & 1) == 1 {
                    merged.merge_entry(history[index].entry.clone())
                } else {
                    Ok(merged)
                }
            })
            .expect("honest history stays within the active-value cap")
    }

    fn strictly_after(left: [u64; 3], right: [u64; 3]) -> bool {
        left.iter().zip(right).all(|(left, right)| left >= &right)
            && left.iter().zip(right).any(|(left, right)| left > &right)
    }

    fn expected_antichain(history: &[HonestEvent]) -> Vec<OpId> {
        let mut active: Vec<_> = history
            .iter()
            .filter(|candidate| {
                !history.iter().any(|other| {
                    other.entry.id() != candidate.entry.id()
                        && strictly_after(other.clock, candidate.clock)
                })
            })
            .map(|event| event.entry.id())
            .collect();
        active.sort_unstable();
        active
    }

    fn active_ids(register: &CausalRegister<TestValue>) -> Vec<OpId> {
        register.active().map(RegisterEntry::id).collect()
    }

    fn register(entries: &[RegisterEntry<TestValue>]) -> CausalRegister<TestValue> {
        entries
            .iter()
            .cloned()
            .try_fold(CausalRegister::empty(), |current, entry| {
                current.merge_entry(entry)
            })
            .expect("bounded test register")
    }

    #[test]
    fn causal_replacement_removes_prior_value() {
        let file = entry(&VersionVector::empty(), 0, TestValue::File("bytes"));
        let tombstone = entry(file.clock(), 0, TestValue::Tombstone);
        let merged = register(&[file, tombstone.clone()]);
        assert_eq!(merged.active().collect::<Vec<_>>(), vec![&tombstone]);
    }

    #[test]
    fn later_dominating_operation_can_arrive_before_its_predecessor() {
        let file = entry(&VersionVector::empty(), 0, TestValue::File("bytes"));
        let tombstone = entry(file.clock(), 0, TestValue::Tombstone);
        let merged = CausalRegister::empty()
            .merge_entry(tombstone.clone())
            .expect("later operation")
            .merge_entry(file)
            .expect("historical delivery");
        assert_eq!(merged.active().collect::<Vec<_>>(), vec![&tombstone]);
    }

    #[test]
    fn concurrent_file_directory_and_tombstone_all_remain() {
        let file = entry(&VersionVector::empty(), 0, TestValue::File("bytes"));
        let directory = entry(&VersionVector::empty(), 1, TestValue::Directory);
        let tombstone = entry(&VersionVector::empty(), 2, TestValue::Tombstone);
        let merged = register(&[file, directory, tombstone]);
        assert_eq!(merged.active_count(), 3);
        assert!(
            merged
                .active()
                .any(|entry| matches!(entry.value(), TestValue::File(_)))
        );
        assert!(
            merged
                .active()
                .any(|entry| matches!(entry.value(), TestValue::Directory))
        );
        assert!(
            merged
                .active()
                .any(|entry| matches!(entry.value(), TestValue::Tombstone))
        );
    }

    #[test]
    fn two_and_three_writer_delivery_permutations_converge() {
        let first = entry(&VersionVector::empty(), 0, TestValue::File("a"));
        let second = entry(&VersionVector::empty(), 1, TestValue::Directory);
        let third = entry(&VersionVector::empty(), 2, TestValue::Tombstone);
        let expected = register(&[first.clone(), second.clone(), third.clone()]);
        for order in [
            [&first, &second, &third],
            [&first, &third, &second],
            [&second, &first, &third],
            [&second, &third, &first],
            [&third, &first, &second],
            [&third, &second, &first],
        ] {
            let result = order
                .into_iter()
                .try_fold(CausalRegister::empty(), |current, entry| {
                    current.merge_entry(entry.clone())
                })
                .expect("permutation merge");
            assert_eq!(result, expected);
        }
        assert_eq!(
            register(&[first.clone(), second.clone()])
                .merge(&expected)
                .expect("merge"),
            expected
        );
    }

    #[test]
    fn valid_history_merges_are_commutative_associative_and_idempotent() {
        let first = register(&[entry(&VersionVector::empty(), 0, TestValue::File("a"))]);
        let second = register(&[entry(&VersionVector::empty(), 1, TestValue::Directory)]);
        let third = register(&[entry(&VersionVector::empty(), 2, TestValue::Tombstone)]);

        assert_eq!(first.merge(&first).expect("idempotent"), first);
        assert_eq!(
            first.merge(&second).expect("left merge"),
            second.merge(&first).expect("right merge")
        );
        assert_eq!(
            first
                .merge(&second)
                .expect("first pair")
                .merge(&third)
                .expect("left association"),
            first
                .merge(&second.merge(&third).expect("second pair"))
                .expect("right association")
        );
    }

    #[test]
    fn same_dot_mismatch_is_rejected_before_dominance() {
        let same_dot = OpId::new(actor(0), 1).expect("dot");
        let short = VersionVector::new([(actor(0), 1)]).expect("short clock");
        let long = VersionVector::new([(actor(0), 1), (actor(1), 1)]).expect("long clock");
        let first = RegisterEntry::from_full_clock(same_dot, short, TestValue::File("first"))
            .expect("first");
        let conflicting = RegisterEntry::from_full_clock(same_dot, long, TestValue::File("second"))
            .expect("conflicting shape");
        assert_eq!(
            CausalRegister::empty()
                .merge_entry(first)
                .expect("first merge")
                .merge_entry(conflicting),
            Err(RegisterError::OpIdMismatch)
        );
    }

    #[test]
    fn equal_clock_with_distinct_dots_is_rejected() {
        let shared_clock =
            VersionVector::new([(actor(0), 1), (actor(1), 1)]).expect("shared clock");
        let first = RegisterEntry::from_full_clock(
            OpId::new(actor(0), 1).expect("first dot"),
            shared_clock.clone(),
            TestValue::File("first"),
        )
        .expect("first shape");
        let second = RegisterEntry::from_full_clock(
            OpId::new(actor(1), 1).expect("second dot"),
            shared_clock,
            TestValue::File("second"),
        )
        .expect("second shape");

        assert_eq!(
            CausalRegister::empty()
                .merge_entry(first)
                .expect("first merge")
                .merge_entry(second),
            Err(RegisterError::EqualClockDifferentDot)
        );
    }

    #[test]
    fn zero_and_overflow_are_rejected_without_mutating_prior_values() {
        assert_eq!(OpId::new(actor(0), 0), Err(RegisterError::ZeroDot));
        let maximum = VersionVector::new([(actor(0), u64::MAX)]).expect("maximum");
        assert_eq!(
            RegisterEntry::publish(&maximum, actor(0), TestValue::File("overflow")),
            Err(RegisterError::VersionVector(
                VersionVectorError::CounterOverflow
            ))
        );
        assert_eq!(maximum.counter(actor(0)), u64::MAX);
    }

    #[test]
    fn active_129th_concurrent_value_fails_atomically() {
        let entries: Vec<_> = (0..=MAX_VERSION_VECTOR_ACTORS)
            .map(|index| entry(&VersionVector::empty(), index as u128, TestValue::File("v")))
            .collect();
        let full = register(&entries[..MAX_VERSION_VECTOR_ACTORS]);
        let before = full.clone();
        assert_eq!(
            full.merge_entry(entries[MAX_VERSION_VECTOR_ACTORS].clone()),
            Err(RegisterError::TooManyActiveValues)
        );
        assert_eq!(full, before);
    }

    #[test]
    fn full_union_can_exceed_bound_when_final_antichain_fits() {
        let initial: Vec<_> = (0..MAX_VERSION_VECTOR_ACTORS)
            .map(|index| {
                entry(
                    &VersionVector::empty(),
                    index as u128,
                    TestValue::File("old"),
                )
            })
            .collect();
        let left = register(&initial);
        let frontier = initial
            .iter()
            .fold(VersionVector::empty(), |frontier, entry| {
                frontier.join(entry.clock()).expect("128 actor frontier")
            });
        let replacement = entry(&frontier, 0, TestValue::File("new"));
        let right = CausalRegister::empty()
            .merge_entry(replacement.clone())
            .expect("right");
        let merged = left
            .merge(&right)
            .expect("reduce complete union before enforcing cap");
        assert_eq!(merged.active().collect::<Vec<_>>(), vec![&replacement]);
    }

    #[test]
    fn reconstructed_entry_requires_its_dot_in_the_clock() {
        let id = OpId::new(actor(0), 2).expect("positive dot");
        assert_eq!(
            RegisterEntry::from_full_clock(id, VersionVector::empty(), TestValue::File("file")),
            Err(RegisterError::DotDoesNotMatchClock)
        );
        assert_eq!(
            RegisterEntry::from_full_clock(
                id,
                VersionVector::new([(actor(0), 1)]).expect("mismatched clock"),
                TestValue::File("file"),
            ),
            Err(RegisterError::DotDoesNotMatchClock)
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn honest_three_writer_delivery_converges_to_independent_antichain(
            script in prop::collection::vec((0_u8..3, any::<u32>(), any::<u8>()), 0..=24),
            seeds in prop::array::uniform3(any::<u32>()),
        ) {
            let history = honest_history(&script);
            let expected = expected_antichain(&history);
            let all_indices = shuffled_indices(history.len(), seeds[0]);
            let mut replicas = [
                CausalRegister::empty(),
                CausalRegister::empty(),
                CausalRegister::empty(),
            ];

            for replica in 0..3 {
                let subset = shuffled_indices(history.len(), seeds[replica])
                    .into_iter()
                    .filter(|index| ((seeds[replica].rotate_left((index % 32) as u32) >> 1) & 1) == 1);
                replicas[replica] = deliver(
                    CausalRegister::empty(),
                    &history,
                    subset,
                    seeds[replica],
                );
            }

            let left_then_middle = replicas[0].merge(&replicas[1]).expect("valid partial union");
            assert_eq!(left_then_middle, replicas[1].merge(&replicas[0]).expect("commutative"));
            assert_eq!(replicas[0].merge(&replicas[0]).expect("idempotent"), replicas[0]);
            assert_eq!(
                left_then_middle.merge(&replicas[2]).expect("left association"),
                replicas[0]
                    .merge(&replicas[1].merge(&replicas[2]).expect("right pair"))
                    .expect("right association"),
            );

            for replica in &replicas {
                let complete = deliver(replica.clone(), &history, all_indices.clone(), seeds[1]);
                assert_eq!(active_ids(&complete), expected);
            }
        }
    }
}
