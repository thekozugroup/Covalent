//! Bounded protocol building blocks for a future folder-sync implementation.
//!
//! These types provide canonical paths, signature checking, causal math,
//! conflict projection, read-only inventories, and private encrypted event
//! storage and immutable encrypted content retention. The replay engine accepts
//! bootstrap-backed membership additions/upgrades and read-only removals;
//! write-loss changes still fail closed. Local file publication requires exact
//! durable content receipts. A Unix applier can journal and verify create-only
//! file/directory placement and adopt matching existing files without changing
//! their modes. No network sync runtime is shipped; replacement,
//! deletion, live authorization and applied/acknowledged frontiers remain open.

/// Read-only causal-history admission checks for a future synchronized path.
pub mod admission;
/// Structural replay state for private filesystem-apply transactions.
#[cfg(unix)]
pub mod apply_machine;
/// Canonical bounded plaintext records for the encrypted apply journal.
pub mod apply_record;
/// Descriptor-relative filesystem creation and verified incumbent adoption.
#[cfg(unix)]
pub mod apply_unix;
/// Immutable protected authority and transport pins for local folder replay.
#[cfg(unix)]
pub mod authority_config;
/// Canonical bounded operation bodies for future folder mutations.
pub mod body;
/// Canonical signed read-only bootstrap permits and candidate receipts.
pub mod bootstrap;
/// Bounded local encryption and keyed locators for immutable sync content.
pub mod content_crypto;
/// Canonical bounded manifests for ordered immutable sync file chunks.
pub mod content_manifest;
/// Verified immutable local retention before file operation publication.
#[cfg(unix)]
pub mod content_store;
/// Closed bounded event envelopes and explicitly untrusted routing hints.
pub mod event;
/// Bounded encrypted append and replay storage for private folder events.
#[cfg(unix)]
pub mod event_log;
/// Canonical signed write-loss proposals, freeze receipts, and aborts.
pub mod freeze;
/// Bounded exact-history checks for causally closed evidence frontiers.
pub mod frontier;
/// Folder and per-install writer identifier types.
pub mod ids;
/// Protected immutable local writer/key installation for one sync folder.
#[cfg(unix)]
pub mod installation;
#[cfg(all(test, unix))]
mod installation_handoff_tests;
/// Bounded authenticated frames for future private synchronization logs.
pub mod log_frame;
/// Replay-derived membership and operation admission with bounded indexes.
#[cfg(unix)]
pub mod machine;
/// Canonical signed epoch records for future folder membership.
pub mod membership;
/// Exact accepted-history validation of full-roster membership transitions.
pub mod membership_transition;
/// Canonical bounded signatures for future folder operation headers.
pub mod operation;
/// Bounded canonical paths and conservative collision hints.
pub mod path;
/// Pure deterministic folder projection for future sync apply planning.
pub mod projection;
/// Durable local operation signing from accepted folder-global history.
#[cfg(unix)]
pub mod publication;
/// Pure multi-value causal register math for a future synchronized path.
pub mod register;
/// Read-only descriptor-anchored local source inventory for future sync.
#[cfg(unix)]
pub mod source_inventory;
/// Descriptor-anchored private local state capabilities for future sync.
#[cfg(unix)]
pub mod state_dir;

use std::collections::BTreeMap;
use std::fmt;

use covalent_protocol::DeviceId;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Maximum distinct actors in one vector.
pub const MAX_VERSION_VECTOR_ACTORS: usize = 128;

/// The causal relation between two version vectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionVectorOrder {
    /// Every counter is identical.
    Equal,
    /// The left vector is causally before the right vector.
    Before,
    /// The left vector is causally after the right vector.
    After,
    /// Each vector has at least one update the other does not dominate.
    Concurrent,
}

/// A rejected vector invariant or arithmetic operation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum VersionVectorError {
    /// Counters are positive causal events; zero is represented by absence.
    #[error("version-vector counters must be positive")]
    ZeroCounter,
    /// The same actor appeared more than once in one canonical vector.
    #[error("version vector contains a duplicate actor")]
    DuplicateActor,
    /// More than the fixed number of actors would make state unbounded.
    #[error("version vector has too many actors")]
    TooManyActors,
    /// Advancing `u64::MAX` would destroy monotonicity.
    #[error("version-vector counter overflow")]
    CounterOverflow,
}

/// A bounded, canonical causal frontier.
///
/// Missing actors have counter zero. Stored counters are always strictly
/// positive. The private `BTreeMap` fixes iteration and serialization order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VersionVector {
    counters: BTreeMap<DeviceId, u64>,
}

impl VersionVector {
    /// Builds a vector after checking every input invariant.
    pub fn new(
        entries: impl IntoIterator<Item = (DeviceId, u64)>,
    ) -> Result<Self, VersionVectorError> {
        let mut counters = BTreeMap::new();
        for (actor, counter) in entries {
            if counter == 0 {
                return Err(VersionVectorError::ZeroCounter);
            }
            if counters.contains_key(&actor) {
                return Err(VersionVectorError::DuplicateActor);
            }
            if counters.len() == MAX_VERSION_VECTOR_ACTORS {
                return Err(VersionVectorError::TooManyActors);
            }
            counters.insert(actor, counter);
        }
        Ok(Self { counters })
    }

    /// Returns the empty causal frontier.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns the counter for `actor`, or zero when the actor is absent.
    #[must_use]
    pub fn counter(&self, actor: DeviceId) -> u64 {
        self.counters.get(&actor).copied().unwrap_or(0)
    }

    /// Returns the number of explicitly represented actors.
    #[must_use]
    pub fn actor_count(&self) -> usize {
        self.counters.len()
    }

    /// Advances one actor without changing this vector.
    ///
    /// The returned counter belongs to the returned vector. Overflow and a new
    /// 129th actor fail before any state is exposed or mutated.
    pub fn checked_advance(&self, actor: DeviceId) -> Result<(Self, u64), VersionVectorError> {
        let previous = self.counter(actor);
        let next = previous
            .checked_add(1)
            .ok_or(VersionVectorError::CounterOverflow)?;
        if previous == 0 && self.counters.len() == MAX_VERSION_VECTOR_ACTORS {
            return Err(VersionVectorError::TooManyActors);
        }
        let mut counters = self.counters.clone();
        counters.insert(actor, next);
        Ok((Self { counters }, next))
    }

    /// Returns the element-wise maximum without changing either input.
    ///
    /// A disjoint merge that would exceed the actor cap returns an error and
    /// leaves both original vectors unchanged.
    pub fn join(&self, other: &Self) -> Result<Self, VersionVectorError> {
        let additional = other
            .counters
            .keys()
            .filter(|actor| !self.counters.contains_key(actor))
            .count();
        if additional > MAX_VERSION_VECTOR_ACTORS.saturating_sub(self.counters.len()) {
            return Err(VersionVectorError::TooManyActors);
        }
        let mut counters = self.counters.clone();
        for (actor, counter) in &other.counters {
            counters
                .entry(*actor)
                .and_modify(|current| *current = (*current).max(*counter))
                .or_insert(*counter);
        }
        Ok(Self { counters })
    }

    /// Compares causal dominance without timestamps or arrival order.
    #[must_use]
    pub fn compare(&self, other: &Self) -> VersionVectorOrder {
        let mut before = false;
        let mut after = false;
        for (actor, counter) in &self.counters {
            match counter.cmp(&other.counter(*actor)) {
                std::cmp::Ordering::Less => before = true,
                std::cmp::Ordering::Greater => after = true,
                std::cmp::Ordering::Equal => {}
            }
        }
        for actor in other.counters.keys() {
            if !self.counters.contains_key(actor) {
                before = true;
            }
        }
        match (before, after) {
            (false, false) => VersionVectorOrder::Equal,
            (true, false) => VersionVectorOrder::Before,
            (false, true) => VersionVectorOrder::After,
            (true, true) => VersionVectorOrder::Concurrent,
        }
    }
}

impl Serialize for VersionVector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("VersionVector", 1)?;
        state.serialize_field("entries", &CanonicalEntries(&self.counters))?;
        state.end()
    }
}

struct CanonicalEntries<'a>(&'a BTreeMap<DeviceId, u64>);

impl Serialize for CanonicalEntries<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for (actor, counter) in self.0 {
            sequence.serialize_element(&CanonicalEntry { actor, counter })?;
        }
        sequence.end()
    }
}

struct CanonicalEntry<'a> {
    actor: &'a DeviceId,
    counter: &'a u64,
}

impl Serialize for CanonicalEntry<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("VersionVectorEntry", 2)?;
        state.serialize_field("actor", self.actor)?;
        state.serialize_field("counter", self.counter)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for VersionVector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct("VersionVector", &["entries"], VectorVisitor)
    }
}

struct VectorVisitor;

impl<'de> Visitor<'de> for VectorVisitor {
    type Value = VersionVector;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a version vector with one entries field")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut entries = None;
        while let Some(field) = map.next_key::<VectorField>()? {
            match field {
                VectorField::Entries if entries.is_none() => {
                    entries = Some(map.next_value_seed(EntriesSeed)?);
                }
                VectorField::Entries => return Err(de::Error::duplicate_field("entries")),
            }
        }
        entries.ok_or_else(|| de::Error::missing_field("entries"))
    }
}

enum VectorField {
    Entries,
}

impl<'de> Deserialize<'de> for VectorField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldVisitor;
        impl Visitor<'_> for FieldVisitor {
            type Value = VectorField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("the entries field")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value {
                    "entries" => Ok(VectorField::Entries),
                    _ => Err(E::unknown_field(value, &["entries"])),
                }
            }
        }
        deserializer.deserialize_identifier(FieldVisitor)
    }
}

struct EntriesSeed;

impl<'de> DeserializeSeed<'de> for EntriesSeed {
    type Value = VersionVector;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(EntriesVisitor)
    }
}

struct EntriesVisitor;

impl<'de> Visitor<'de> for EntriesVisitor {
    type Value = VersionVector;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("at most 128 unique positive version-vector entries")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut counters = BTreeMap::new();
        while let Some((actor, counter)) = sequence.next_element_seed(EntrySeed)? {
            if counter == 0 {
                return Err(de::Error::custom(VersionVectorError::ZeroCounter));
            }
            if counters.contains_key(&actor) {
                return Err(de::Error::custom(VersionVectorError::DuplicateActor));
            }
            if counters.len() == MAX_VERSION_VECTOR_ACTORS {
                return Err(de::Error::custom(VersionVectorError::TooManyActors));
            }
            counters.insert(actor, counter);
        }
        Ok(VersionVector { counters })
    }
}

struct EntrySeed;

impl<'de> DeserializeSeed<'de> for EntrySeed {
    type Value = (DeviceId, u64);

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct("VersionVectorEntry", &["actor", "counter"], EntryVisitor)
    }
}

struct EntryVisitor;

impl<'de> Visitor<'de> for EntryVisitor {
    type Value = (DeviceId, u64);

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a version-vector entry with actor and positive counter")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut actor = None;
        let mut counter = None;
        while let Some(field) = map.next_key::<EntryField>()? {
            match field {
                EntryField::Actor if actor.is_none() => actor = Some(map.next_value()?),
                EntryField::Counter if counter.is_none() => counter = Some(map.next_value()?),
                EntryField::Actor => return Err(de::Error::duplicate_field("actor")),
                EntryField::Counter => return Err(de::Error::duplicate_field("counter")),
            }
        }
        Ok((
            actor.ok_or_else(|| de::Error::missing_field("actor"))?,
            counter.ok_or_else(|| de::Error::missing_field("counter"))?,
        ))
    }
}

enum EntryField {
    Actor,
    Counter,
}

impl<'de> Deserialize<'de> for EntryField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldVisitor;
        impl Visitor<'_> for FieldVisitor {
            type Value = EntryField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an actor or counter field")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value {
                    "actor" => Ok(EntryField::Actor),
                    "counter" => Ok(EntryField::Counter),
                    _ => Err(E::unknown_field(value, &["actor", "counter"])),
                }
            }
        }
        deserializer.deserialize_identifier(FieldVisitor)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn actor(number: u128) -> DeviceId {
        DeviceId::from_uuid(uuid::Uuid::from_u128(number + 1))
    }

    fn vector(counters: [u16; 3]) -> VersionVector {
        VersionVector::new(
            counters
                .into_iter()
                .enumerate()
                .filter_map(|(index, counter)| {
                    (counter != 0).then_some((actor(index as u128), u64::from(counter)))
                }),
        )
        .expect("bounded test vector")
    }

    #[test]
    fn canonical_serialization_is_order_independent() {
        let left = VersionVector::new([(actor(2), 4), (actor(0), 3)]).expect("left");
        let right = VersionVector::new([(actor(0), 3), (actor(2), 4)]).expect("right");
        assert_eq!(
            serde_json::to_string(&left).expect("serialize"),
            serde_json::to_string(&right).expect("serialize")
        );
    }

    #[test]
    fn failed_disjoint_merge_and_overflow_leave_inputs_unchanged() {
        let left = VersionVector::new(
            (0..MAX_VERSION_VECTOR_ACTORS).map(|index| (actor(index as u128), 1)),
        )
        .expect("full left");
        let right = VersionVector::new([(actor(129), 1)]).expect("right");
        let left_before = left.clone();
        let right_before = right.clone();
        assert_eq!(left.join(&right), Err(VersionVectorError::TooManyActors));
        assert_eq!(left, left_before);
        assert_eq!(right, right_before);

        let maximum = VersionVector::new([(actor(0), u64::MAX)]).expect("maximum");
        let maximum_before = maximum.clone();
        assert_eq!(
            maximum.checked_advance(actor(0)),
            Err(VersionVectorError::CounterOverflow)
        );
        assert_eq!(maximum, maximum_before);
    }

    #[test]
    fn deserialization_rejects_duplicate_zero_and_excess_actors() {
        let one = actor(1);
        let duplicate = format!(
            r#"{{"entries":[{{"actor":"{one}","counter":1}},{{"actor":"{one}","counter":2}}]}}"#
        );
        assert!(serde_json::from_str::<VersionVector>(&duplicate).is_err());
        let zero = format!(r#"{{"entries":[{{"actor":"{}","counter":0}}]}}"#, actor(2));
        assert!(serde_json::from_str::<VersionVector>(&zero).is_err());
        let entries: Vec<_> = (0..=MAX_VERSION_VECTOR_ACTORS)
            .map(|index| serde_json::json!({"actor": actor(index as u128), "counter": 1}))
            .collect();
        assert!(
            serde_json::from_value::<VersionVector>(serde_json::json!({"entries": entries}))
                .is_err()
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn join_is_commutative_associative_and_idempotent(
            left in prop::array::uniform3(0_u16..500),
            middle in prop::array::uniform3(0_u16..500),
            right in prop::array::uniform3(0_u16..500),
        ) {
            let left = vector(left);
            let middle = vector(middle);
            let right = vector(right);
            prop_assert_eq!(left.join(&middle)?, middle.join(&left)?);
            prop_assert_eq!(left.join(&left)?, left.clone());
            prop_assert_eq!(left.join(&middle)?.join(&right)?, left.join(&middle.join(&right)?)?);
        }

        #[test]
        fn comparison_is_dual_and_respects_join_partial_order(
            left in prop::array::uniform3(0_u16..500),
            right in prop::array::uniform3(0_u16..500),
        ) {
            let left = vector(left);
            let right = vector(right);
            let forward = left.compare(&right);
            let reverse = right.compare(&left);
            let expected_reverse = match forward {
                VersionVectorOrder::Equal => VersionVectorOrder::Equal,
                VersionVectorOrder::Before => VersionVectorOrder::After,
                VersionVectorOrder::After => VersionVectorOrder::Before,
                VersionVectorOrder::Concurrent => VersionVectorOrder::Concurrent,
            };
            prop_assert_eq!(reverse, expected_reverse);
            let joined = left.join(&right)?;
            prop_assert!(matches!(left.compare(&joined), VersionVectorOrder::Equal | VersionVectorOrder::Before));
            prop_assert!(matches!(right.compare(&joined), VersionVectorOrder::Equal | VersionVectorOrder::Before));
            if forward == VersionVectorOrder::Before {
                prop_assert_eq!(joined, right);
            }
        }
    }
}
