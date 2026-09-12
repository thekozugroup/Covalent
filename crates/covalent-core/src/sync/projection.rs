//! Deterministic, pure projection of admitted folder-path registers.
//!
//! Callers must supply one authoritative, locked durable-log snapshot. Checking
//! that clocks are at or below `frontier` cannot prove snapshot completeness.
//! This module does not authenticate, authorize, apply, scan, cache, probe a
//! target, or emit operations. Filesystem apply must still make actual target
//! capability and no-clobber probes before every mutation.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use super::body::{EntryValue, FileContent};
use super::path::{MAX_SYNC_PATH_COMPONENT_BYTES, MAX_SYNC_PATH_DEPTH, SyncPath};
use super::register::{CausalRegister, OpId, RegisterEntry};
use super::{VersionVector, VersionVectorOrder};

const CONFLICT_PREFIX: &str = " (conflict-";
const CONFLICT_SUFFIX_BYTES: usize = 61;

/// Explicit resource limits for one pure projection request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionLimits {
    pub maximum_paths: usize,
    pub maximum_active_versions: usize,
    pub maximum_output_entries: usize,
    pub maximum_aggregate_path_bytes: usize,
}

/// One deterministic file placement requiring later content staging and apply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilePlacement {
    source_path: SyncPath,
    dot: OpId,
    content: FileContent,
    target_path: SyncPath,
}

impl FilePlacement {
    #[must_use]
    pub const fn dot(&self) -> OpId {
        self.dot
    }
    #[must_use]
    pub const fn content(&self) -> FileContent {
        self.content
    }
    #[must_use]
    pub fn source_path(&self) -> &SyncPath {
        &self.source_path
    }
    #[must_use]
    pub fn target_path(&self) -> &SyncPath {
        &self.target_path
    }
}

/// One physical directory desired by a projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DirectoryPlacement {
    Explicit {
        source_path: SyncPath,
        dot: OpId,
        target_path: SyncPath,
    },
    Structural {
        target_path: SyncPath,
    },
}

impl DirectoryPlacement {
    #[must_use]
    pub fn target_path(&self) -> &SyncPath {
        match self {
            Self::Explicit { target_path, .. } | Self::Structural { target_path } => target_path,
        }
    }
}

/// A retained active branch that has no safe physical placement in this plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingProjection {
    source_path: SyncPath,
    dot: OpId,
    content: Option<FileContent>,
    reason: PendingReason,
}

impl PendingProjection {
    #[must_use]
    pub const fn dot(&self) -> OpId {
        self.dot
    }
    #[must_use]
    pub const fn content(&self) -> Option<FileContent> {
        self.content
    }
    #[must_use]
    pub const fn reason(&self) -> PendingReason {
        self.reason
    }
    #[must_use]
    pub fn source_path(&self) -> &SyncPath {
        &self.source_path
    }
}

/// Why an active branch remains visible but is not physically placed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingReason {
    UnrepresentableConflictName,
    ReservedOriginalPath,
    ExactTargetCollision,
    BlockedByPlacedFile,
}

/// A visible unresolved state that does not itself make a placement unsafe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictMarker {
    source_path: SyncPath,
    dots: Vec<OpId>,
    reason: ConflictReason,
}

impl ConflictMarker {
    #[must_use]
    pub fn source_path(&self) -> &SyncPath {
        &self.source_path
    }
    #[must_use]
    pub fn dots(&self) -> &[OpId] {
        &self.dots
    }
    #[must_use]
    pub const fn reason(&self) -> ConflictReason {
        self.reason
    }
}

/// A durable active-state conflict that must remain visible to the user.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ConflictReason {
    ConcurrentValues,
    FileWithLiveChildren,
    TombstoneWithLiveChildren,
}

/// The immutable projection result, sorted by target/source path and dot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Projection {
    files: Vec<FilePlacement>,
    directories: Vec<DirectoryPlacement>,
    pending: Vec<PendingProjection>,
    conflicts: Vec<ConflictMarker>,
}

impl Projection {
    #[must_use]
    pub fn files(&self) -> &[FilePlacement] {
        &self.files
    }
    #[must_use]
    pub fn directories(&self) -> &[DirectoryPlacement] {
        &self.directories
    }
    #[must_use]
    pub fn pending(&self) -> &[PendingProjection] {
        &self.pending
    }
    #[must_use]
    pub fn conflicts(&self) -> &[ConflictMarker] {
        &self.conflicts
    }
}

/// Projection failure before any result is exposed; inputs are always borrowed.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProjectionError {
    #[error("projection input exceeds a configured bound")]
    InputLimit,
    #[error("projection output exceeds a configured bound")]
    OutputLimit,
    #[error("an active clock is not at or below the supplied frontier")]
    ClockBeyondFrontier,
}

#[derive(Clone)]
enum CandidateKind {
    File(FileContent),
    Directory,
}

#[derive(Clone)]
struct Candidate {
    source: SyncPath,
    dot: OpId,
    target: Option<SyncPath>,
    failure: Option<PendingReason>,
    kind: CandidateKind,
}

/// Projects active branches from an already-admitted, complete snapshot.
pub fn project(
    paths: &BTreeMap<SyncPath, CausalRegister<EntryValue>>,
    frontier: &VersionVector,
    limits: ProjectionLimits,
) -> Result<Projection, ProjectionError> {
    if paths.len() > limits.maximum_paths {
        return Err(ProjectionError::InputLimit);
    }
    let mut active_versions = 0_usize;
    let mut input_bytes = 0_usize;
    for (path, register) in paths {
        input_bytes = input_bytes
            .checked_add(path.as_str().len())
            .ok_or(ProjectionError::InputLimit)?;
        active_versions = active_versions
            .checked_add(register.active_count())
            .ok_or(ProjectionError::InputLimit)?;
        if input_bytes > limits.maximum_aggregate_path_bytes
            || active_versions > limits.maximum_active_versions
        {
            return Err(ProjectionError::InputLimit);
        }
        for entry in register.active() {
            if matches!(
                entry.clock().compare(frontier),
                VersionVectorOrder::After | VersionVectorOrder::Concurrent
            ) {
                return Err(ProjectionError::ClockBeyondFrontier);
            }
        }
    }

    let reserved: BTreeSet<_> = paths.keys().cloned().collect();
    let file_winners: BTreeMap<_, _> = paths
        .iter()
        .filter_map(|(path, register)| {
            register.active().next().and_then(|entry| {
                matches!(entry.value(), EntryValue::File(_)).then_some((path.clone(), entry.id()))
            })
        })
        .collect();
    // Each input path can contribute at most `MAX_SYNC_PATH_DEPTH` distinct
    // ancestors. Reserve each unique clone before inserting it, so this
    // temporary index is bounded even though it is not part of the output.
    // Saturation retains the ordinary per-insertion overflow check for a
    // deliberately maximal caller limit without rejecting a small request.
    let live_child_budget = limits
        .maximum_aggregate_path_bytes
        .saturating_mul(MAX_SYNC_PATH_DEPTH);
    let mut live_child_bytes = 0_usize;
    let mut live_child_ancestors = BTreeSet::new();
    for (path, register) in paths {
        // A tombstone can sort before a concurrent file. The file is still
        // retained at a conflict sibling, so its ancestors must remain
        // visibly conflicted too. A non-winning directory has no physical
        // directory placement of its own.
        let has_active_file = register
            .active()
            .any(|entry| matches!(entry.value(), EntryValue::File(_)));
        let winning_directory = register
            .active()
            .next()
            .is_some_and(|entry| matches!(entry.value(), EntryValue::Directory));
        if has_active_file || winning_directory {
            for ancestor in ancestors(path) {
                if !live_child_ancestors.contains(&ancestor) {
                    live_child_bytes = live_child_bytes
                        .checked_add(ancestor.as_str().len())
                        .ok_or(ProjectionError::InputLimit)?;
                    if live_child_bytes > live_child_budget {
                        return Err(ProjectionError::InputLimit);
                    }
                    live_child_ancestors.insert(ancestor);
                }
            }
        }
    }
    let mut candidates = Vec::new();
    let mut conflicts = Vec::new();
    let mut output_entries = 0_usize;
    let mut output_bytes = 0_usize;
    for (source, register) in paths {
        let has_live_child = live_child_ancestors.contains(source);
        let base = base_target(source, &file_winners, &reserved);
        if register.active_count() > 1 {
            reserve_output(&mut output_entries, &mut output_bytes, source, None, limits)?;
            conflicts.push(ConflictMarker {
                source_path: source.clone(),
                dots: register.active().map(RegisterEntry::id).collect(),
                reason: ConflictReason::ConcurrentValues,
            });
        }
        if register.active_count() == 1
            && register
                .active()
                .next()
                .is_some_and(|entry| matches!(entry.value(), EntryValue::File(_)))
            && has_live_child
        {
            reserve_output(&mut output_entries, &mut output_bytes, source, None, limits)?;
            conflicts.push(ConflictMarker {
                source_path: source.clone(),
                dots: vec![register.active().next().expect("active winner").id()],
                reason: ConflictReason::FileWithLiveChildren,
            });
        }
        if register
            .active()
            .next()
            .is_some_and(|entry| matches!(entry.value(), EntryValue::Tombstone))
            && has_live_child
        {
            reserve_output(&mut output_entries, &mut output_bytes, source, None, limits)?;
            conflicts.push(ConflictMarker {
                source_path: source.clone(),
                dots: vec![register.active().next().expect("active winner").id()],
                reason: ConflictReason::TombstoneWithLiveChildren,
            });
        }
        for (index, entry) in register.active().enumerate() {
            let winner = index == 0;
            let kind = entry.value();
            let (target, failure, candidate_kind) = match kind {
                EntryValue::File(content) => {
                    let target = if winner {
                        base.as_ref().ok().cloned()
                    } else {
                        base.as_ref()
                            .ok()
                            .and_then(|path| conflict_sibling(path, entry.id()))
                    };
                    let failure = base.as_ref().err().copied().or_else(|| {
                        target
                            .is_none()
                            .then_some(PendingReason::UnrepresentableConflictName)
                    });
                    (target, failure, CandidateKind::File(*content))
                }
                EntryValue::Directory if winner => (
                    base.as_ref().ok().cloned(),
                    base.as_ref().err().copied(),
                    CandidateKind::Directory,
                ),
                EntryValue::Directory | EntryValue::Tombstone => continue,
            };
            reserve_output(
                &mut output_entries,
                &mut output_bytes,
                source,
                target.as_ref(),
                limits,
            )?;
            candidates.push(Candidate {
                source: source.clone(),
                dot: entry.id(),
                target,
                failure,
                kind: candidate_kind,
            });
        }
    }

    let mut pending = Vec::new();
    let mut placed = Vec::new();
    for candidate in candidates {
        match candidate.kind.clone() {
            CandidateKind::File(_) | CandidateKind::Directory => {
                let content = content_from_kind(&candidate.kind);
                let Some(target) = candidate.target.clone() else {
                    let reason = candidate
                        .failure
                        .unwrap_or(PendingReason::UnrepresentableConflictName);
                    pending.push(pending_from(candidate, content, reason));
                    continue;
                };
                let synthetic = target != candidate.source;
                if synthetic && reserved.contains(&target) {
                    pending.push(pending_from(
                        candidate,
                        content,
                        PendingReason::ReservedOriginalPath,
                    ));
                } else {
                    placed.push((candidate, target));
                }
            }
        }
    }

    let duplicate_targets: BTreeSet<_> = placed
        .iter()
        .fold(
            BTreeMap::<SyncPath, usize>::new(),
            |mut count, (_, target)| {
                *count.entry(target.clone()).or_default() += 1;
                count
            },
        )
        .into_iter()
        .filter_map(|(target, count)| (count > 1).then_some(target))
        .collect();
    let mut files = Vec::new();
    let mut explicit_directories = Vec::new();
    for (candidate, target) in placed {
        if duplicate_targets.contains(&target) {
            let content = content_from_kind(&candidate.kind);
            pending.push(pending_from(
                candidate,
                content,
                PendingReason::ExactTargetCollision,
            ));
        } else if let CandidateKind::File(content) = &candidate.kind {
            files.push(FilePlacement {
                source_path: candidate.source,
                dot: candidate.dot,
                content: *content,
                target_path: target,
            });
        } else {
            explicit_directories.push(DirectoryPlacement::Explicit {
                source_path: candidate.source,
                dot: candidate.dot,
                target_path: target,
            });
        }
    }
    files.sort_by(|left, right| {
        left.target_path
            .cmp(&right.target_path)
            .then(left.dot.cmp(&right.dot))
    });
    let file_targets: BTreeSet<_> = files.iter().map(|file| file.target_path.clone()).collect();
    let mut retained_files = Vec::new();
    for file in files {
        if ancestors(&file.target_path).any(|ancestor| file_targets.contains(&ancestor)) {
            pending.push(PendingProjection {
                source_path: file.source_path,
                dot: file.dot,
                content: Some(file.content),
                reason: PendingReason::BlockedByPlacedFile,
            });
        } else {
            retained_files.push(file);
        }
    }
    let retained_file_targets: BTreeSet<_> = retained_files
        .iter()
        .map(|file| file.target_path.clone())
        .collect();
    let mut directories = Vec::new();
    for directory in explicit_directories {
        if ancestors(directory.target_path())
            .any(|ancestor| retained_file_targets.contains(&ancestor))
        {
            let DirectoryPlacement::Explicit {
                source_path, dot, ..
            } = directory
            else {
                unreachable!()
            };
            pending.push(PendingProjection {
                source_path,
                dot,
                content: None,
                reason: PendingReason::BlockedByPlacedFile,
            });
        } else {
            directories.push(directory);
        }
    }
    let existing_directories: BTreeSet<_> = directories
        .iter()
        .map(|directory| directory.target_path().clone())
        .collect();
    let mut structural = BTreeSet::new();
    for target in retained_files
        .iter()
        .map(FilePlacement::target_path)
        .chain(directories.iter().map(DirectoryPlacement::target_path))
    {
        for ancestor in ancestors(target) {
            if !retained_file_targets.contains(&ancestor)
                && !existing_directories.contains(&ancestor)
                && !structural.contains(&ancestor)
            {
                reserve_output(
                    &mut output_entries,
                    &mut output_bytes,
                    &ancestor,
                    None,
                    limits,
                )?;
                structural.insert(ancestor);
            }
        }
    }
    for target_path in structural {
        directories.push(DirectoryPlacement::Structural { target_path });
    }
    directories.sort_by(|left, right| left.target_path().cmp(right.target_path()));
    pending.sort_by(|left, right| {
        left.source_path
            .cmp(&right.source_path)
            .then(left.dot.cmp(&right.dot))
            .then(left.reason.cmp(&right.reason))
    });
    conflicts.sort_by(|left, right| {
        left.source_path
            .cmp(&right.source_path)
            .then(left.reason.cmp(&right.reason))
    });
    Ok(Projection {
        files: retained_files,
        directories,
        pending,
        conflicts,
    })
}

fn content_from_kind(kind: &CandidateKind) -> Option<FileContent> {
    if let CandidateKind::File(content) = kind {
        Some(*content)
    } else {
        None
    }
}
fn pending_from(
    candidate: Candidate,
    content: Option<FileContent>,
    reason: PendingReason,
) -> PendingProjection {
    PendingProjection {
        source_path: candidate.source,
        dot: candidate.dot,
        content,
        reason,
    }
}

fn reserve_output(
    entries: &mut usize,
    bytes: &mut usize,
    source: &SyncPath,
    target: Option<&SyncPath>,
    limits: ProjectionLimits,
) -> Result<(), ProjectionError> {
    *entries = entries.checked_add(1).ok_or(ProjectionError::OutputLimit)?;
    *bytes = bytes
        .checked_add(source.as_str().len())
        .ok_or(ProjectionError::OutputLimit)?;
    if let Some(target) = target {
        *bytes = bytes
            .checked_add(target.as_str().len())
            .ok_or(ProjectionError::OutputLimit)?;
    }
    if *entries > limits.maximum_output_entries || *bytes > limits.maximum_aggregate_path_bytes {
        Err(ProjectionError::OutputLimit)
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn is_strict_descendant(path: &SyncPath, ancestor: &SyncPath) -> bool {
    path.as_str()
        .strip_prefix(ancestor.as_str())
        .is_some_and(|rest| rest.starts_with('/'))
}

fn ancestors(path: &SyncPath) -> impl Iterator<Item = SyncPath> + '_ {
    path.as_str()
        .match_indices('/')
        .filter_map(|(index, _)| SyncPath::from_wire(&path.as_str()[..index]).ok())
}

fn base_target(
    source: &SyncPath,
    file_winners: &BTreeMap<SyncPath, OpId>,
    reserved: &BTreeSet<SyncPath>,
) -> Result<SyncPath, PendingReason> {
    let original: Vec<_> = source.components().collect();
    let mut target: Vec<String> = Vec::with_capacity(original.len());
    let mut transformed_prefix = false;
    for (index, component) in original.iter().enumerate() {
        if index > 0 {
            let ancestor = SyncPath::from_wire(original[..index].join("/"))
                .map_err(|_| PendingReason::UnrepresentableConflictName)?;
            if let Some(dot) = file_winners.get(&ancestor) {
                let prior = target
                    .last()
                    .ok_or(PendingReason::UnrepresentableConflictName)?
                    .clone();
                *target
                    .last_mut()
                    .ok_or(PendingReason::UnrepresentableConflictName)? =
                    conflict_component(&prior, *dot)
                        .ok_or(PendingReason::UnrepresentableConflictName)?;
                let prefix = SyncPath::from_wire(target.join("/"))
                    .map_err(|_| PendingReason::UnrepresentableConflictName)?;
                if reserved.contains(&prefix) {
                    return Err(PendingReason::ReservedOriginalPath);
                }
                transformed_prefix = true;
            }
        }
        target.push((*component).to_owned());
        // A renamed ancestor changes every later prefix, not only the
        // component at which the original file blocker was found.
        if transformed_prefix {
            let prefix = SyncPath::from_wire(target.join("/"))
                .map_err(|_| PendingReason::UnrepresentableConflictName)?;
            if reserved.contains(&prefix) {
                return Err(PendingReason::ReservedOriginalPath);
            }
        }
    }
    SyncPath::from_wire(target.join("/")).map_err(|_| PendingReason::UnrepresentableConflictName)
}

fn conflict_sibling(path: &SyncPath, dot: OpId) -> Option<SyncPath> {
    let mut components: Vec<_> = path.components().map(str::to_owned).collect();
    let prior = components.last()?.clone();
    *components.last_mut()? = conflict_component(&prior, dot)?;
    SyncPath::from_wire(components.join("/")).ok()
}

fn conflict_component(name: &str, dot: OpId) -> Option<String> {
    let actor = dot.actor().to_string().replace('-', "");
    let suffix = format!("{CONFLICT_PREFIX}{actor}-{:016x})", dot.counter());
    debug_assert_eq!(suffix.len(), CONFLICT_SUFFIX_BYTES);
    let extension_start = name
        .rfind('.')
        .filter(|index| *index > 0 && *index + 1 < name.len());
    let (prefix, extension) = extension_start.map_or((name, ""), |index| name.split_at(index));
    let prefix_bytes = MAX_SYNC_PATH_COMPONENT_BYTES.checked_sub(suffix.len() + extension.len())?;
    let mut safe = String::new();
    for character in prefix.chars() {
        if safe.len().checked_add(character.len_utf8())? > prefix_bytes {
            break;
        }
        safe.push(character);
    }
    (!safe.is_empty()).then(|| format!("{safe}{suffix}{extension}"))
}

impl Ord for PendingReason {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}
impl PartialOrd for PendingReason {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::body::{ContentDigest, EntryValue};
    use crate::sync::register::RegisterEntry;
    use covalent_protocol::DeviceId;
    use proptest::prelude::*;

    fn actor(number: u128) -> DeviceId {
        DeviceId::from_uuid(uuid::Uuid::from_u128(number + 1))
    }
    fn path(value: &str) -> SyncPath {
        SyncPath::from_wire(value).expect("test path")
    }
    fn content(number: u8) -> FileContent {
        FileContent::new(
            ContentDigest::from_bytes(*blake3::hash(&[number]).as_bytes()),
            u64::from(number) + 1,
            false,
        )
        .expect("file")
    }
    fn entry(number: u128, value: EntryValue) -> RegisterEntry<EntryValue> {
        RegisterEntry::publish(&VersionVector::empty(), actor(number), value).expect("entry")
    }
    fn register(entries: &[RegisterEntry<EntryValue>]) -> CausalRegister<EntryValue> {
        entries
            .iter()
            .cloned()
            .try_fold(CausalRegister::empty(), |register, entry| {
                register.merge_entry(entry)
            })
            .expect("register")
    }
    fn frontier(entries: &[RegisterEntry<EntryValue>]) -> VersionVector {
        entries
            .iter()
            .try_fold(VersionVector::empty(), |frontier, entry| {
                frontier.join(entry.clock())
            })
            .expect("frontier")
    }
    fn limits() -> ProjectionLimits {
        ProjectionLimits {
            maximum_paths: 64,
            maximum_active_versions: 128,
            maximum_output_entries: 512,
            maximum_aggregate_path_bytes: 1 << 20,
        }
    }
    fn project_rows(rows: Vec<(&str, Vec<RegisterEntry<EntryValue>>)>) -> Projection {
        let all: Vec<_> = rows
            .iter()
            .flat_map(|(_, entries)| entries.clone())
            .collect();
        let paths = rows
            .into_iter()
            .map(|(name, entries)| (path(name), register(&entries)))
            .collect();
        project(&paths, &frontier(&all), limits()).expect("projection")
    }
    fn assert_no_file_ancestor(projection: &Projection) {
        for file in projection.files() {
            assert!(!projection.files().iter().any(|other| other != file
                && is_strict_descendant(other.target_path(), file.target_path())));
            assert!(
                !projection
                    .directories()
                    .iter()
                    .any(|directory| is_strict_descendant(
                        directory.target_path(),
                        file.target_path()
                    ))
            );
        }
    }

    #[test]
    fn conflict_suffix_preserves_extensions_and_dotfiles() {
        let file = entry(1, EntryValue::File(content(1)));
        let tombstone = entry(0, EntryValue::Tombstone);
        let projection = project_rows(vec![
            ("report.txt", vec![tombstone, file.clone()]),
            (
                ".env",
                vec![
                    entry(2, EntryValue::File(content(2))),
                    entry(3, EntryValue::File(content(3))),
                ],
            ),
        ]);
        assert!(
            projection
                .files()
                .iter()
                .any(|file| file.target_path().as_str().ends_with(").txt"))
        );
        assert!(
            projection
                .files()
                .iter()
                .any(|file| file.target_path().as_str().starts_with(".env (conflict-"))
        );
        assert_eq!(
            projection.files().len()
                + projection
                    .pending()
                    .iter()
                    .filter(|pending| pending.content().is_some())
                    .count(),
            3
        );
    }

    #[test]
    fn ancestor_files_relocate_descendants_once_per_original_blocker() {
        let projection = project_rows(vec![
            ("a", vec![entry(0, EntryValue::File(content(1)))]),
            ("a/b", vec![entry(1, EntryValue::File(content(2)))]),
            ("a/b/c", vec![entry(2, EntryValue::File(content(3)))]),
        ]);
        assert_eq!(projection.files()[0].target_path().as_str(), "a");
        assert!(
            projection
                .files()
                .iter()
                .any(|file| file.target_path().as_str().contains("a (conflict-"))
        );
        assert_no_file_ancestor(&projection);
    }

    #[test]
    fn file_with_live_descendants_is_visible_even_without_a_same_path_conflict() {
        let projection = project_rows(vec![
            ("a", vec![entry(0, EntryValue::File(content(1)))]),
            ("a/child", vec![entry(1, EntryValue::File(content(2)))]),
        ]);
        assert!(projection.conflicts().iter().any(|conflict| {
            conflict.source_path().as_str() == "a"
                && conflict.reason() == ConflictReason::FileWithLiveChildren
        }));
        assert_no_file_ancestor(&projection);
    }

    #[test]
    fn directory_tombstone_keeps_live_child_and_marks_history() {
        let projection = project_rows(vec![
            ("directory", vec![entry(0, EntryValue::Tombstone)]),
            (
                "directory/live",
                vec![entry(1, EntryValue::File(content(4)))],
            ),
        ]);
        assert!(
            projection
                .files()
                .iter()
                .any(|file| file.target_path().as_str() == "directory/live")
        );
        assert!(
            projection
                .conflicts()
                .iter()
                .any(|conflict| conflict.reason() == ConflictReason::TombstoneWithLiveChildren)
        );
        assert!(projection.directories().iter().any(|directory| matches!(directory, DirectoryPlacement::Structural { target_path } if target_path.as_str() == "directory")));
    }

    #[test]
    fn tombstoned_parent_with_concurrent_file_and_live_child_keeps_the_delete_conflict() {
        let projection = project_rows(vec![
            (
                "directory",
                vec![
                    entry(0, EntryValue::Tombstone),
                    entry(1, EntryValue::File(content(3))),
                ],
            ),
            (
                "directory/live",
                vec![entry(2, EntryValue::File(content(4)))],
            ),
        ]);
        assert!(projection.files().iter().any(|file| {
            file.source_path().as_str() == "directory" && file.target_path().as_str() != "directory"
        }));
        assert!(
            projection
                .files()
                .iter()
                .any(|file| file.source_path().as_str() == "directory/live")
        );
        assert!(projection.conflicts().iter().any(|conflict| {
            conflict.source_path().as_str() == "directory"
                && conflict.reason() == ConflictReason::TombstoneWithLiveChildren
        }));
    }

    #[test]
    fn reserves_tombstoned_original_conflict_lookalike_and_limits_are_atomic() {
        let loser = entry(1, EntryValue::File(content(7)));
        let winner = entry(0, EntryValue::File(content(8)));
        let synthetic = conflict_sibling(&path("photo.jpg"), loser.id()).expect("synthetic path");
        let all = vec![
            winner.clone(),
            loser.clone(),
            entry(2, EntryValue::Tombstone),
        ];
        let paths = BTreeMap::from([
            (path("photo.jpg"), register(&[winner, loser])),
            (synthetic, register(&[entry(2, EntryValue::Tombstone)])),
        ]);
        let before = paths.clone();
        let projection = project(&paths, &frontier(&all), limits()).expect("projection");
        assert!(
            projection
                .pending()
                .iter()
                .any(|pending| pending.content().is_some()
                    && pending.reason() == PendingReason::ReservedOriginalPath)
        );
        assert_eq!(paths, before);
        assert_eq!(
            project(
                &paths,
                &frontier(&all),
                ProjectionLimits {
                    maximum_paths: 1,
                    ..limits()
                }
            ),
            Err(ProjectionError::InputLimit)
        );
    }

    #[test]
    fn huge_extension_is_pending_instead_of_stripped_or_renamed() {
        let name = format!("x.{}", "e".repeat(253));
        let projection = project_rows(vec![(
            name.as_str(),
            vec![
                entry(0, EntryValue::File(content(1))),
                entry(1, EntryValue::File(content(2))),
            ],
        )]);
        assert_eq!(projection.files().len(), 1);
        assert!(
            projection
                .pending()
                .iter()
                .any(|pending| pending.content().is_some()
                    && pending.reason() == PendingReason::UnrepresentableConflictName)
        );
    }

    #[test]
    fn ordinary_and_all_tombstoned_tree_deletions_are_clean() {
        let ordinary = project_rows(vec![("gone", vec![entry(0, EntryValue::Tombstone)])]);
        assert!(ordinary.files().is_empty());
        assert!(ordinary.directories().is_empty());
        assert!(ordinary.pending().is_empty());
        assert!(ordinary.conflicts().is_empty());
        let tree = project_rows(vec![
            ("gone", vec![entry(0, EntryValue::Tombstone)]),
            ("gone/child", vec![entry(1, EntryValue::Tombstone)]),
        ]);
        assert!(tree.files().is_empty());
        assert!(tree.directories().is_empty());
        assert!(tree.pending().is_empty());
        assert!(tree.conflicts().is_empty());
    }

    #[test]
    fn file_conflict_keeps_both_placements_and_one_visible_marker() {
        let projection = project_rows(vec![(
            "same.txt",
            vec![
                entry(0, EntryValue::File(content(1))),
                entry(1, EntryValue::File(content(2))),
            ],
        )]);
        assert_eq!(projection.files().len(), 2);
        assert!(projection.pending().is_empty());
        assert_eq!(projection.conflicts().len(), 1);
        assert_eq!(
            projection.conflicts()[0].reason(),
            ConflictReason::ConcurrentValues
        );
    }

    #[test]
    fn reserved_transformed_prefix_blocks_entire_child_subtree_without_structural_alias() {
        let blocker = entry(0, EntryValue::File(content(1)));
        let reserved_name = conflict_component("a", blocker.id()).expect("name");
        let projection = project_rows(vec![
            ("a", vec![blocker]),
            ("a/child", vec![entry(1, EntryValue::Directory)]),
            (
                reserved_name.as_str(),
                vec![entry(2, EntryValue::Tombstone)],
            ),
        ]);
        assert!(projection.pending().iter().any(|pending| {
            pending.source_path().as_str() == "a/child"
                && pending.reason() == PendingReason::ReservedOriginalPath
        }));
        assert!(
            !projection
                .directories()
                .iter()
                .any(|directory| directory.target_path().as_str() == reserved_name)
        );
    }

    #[test]
    fn reserved_prefix_below_a_renamed_ancestor_blocks_the_later_mapped_subtree() {
        let blocker = entry(0, EntryValue::File(content(1)));
        let renamed = conflict_component("a", blocker.id()).expect("renamed ancestor");
        let reserved_prefix = format!("{renamed}/b");
        let projection = project_rows(vec![
            ("a", vec![blocker]),
            ("a/b/c", vec![entry(1, EntryValue::File(content(2)))]),
            (
                reserved_prefix.as_str(),
                vec![entry(2, EntryValue::Directory)],
            ),
        ]);
        assert!(projection.pending().iter().any(|pending| {
            pending.source_path().as_str() == "a/b/c"
                && pending.reason() == PendingReason::ReservedOriginalPath
                && pending.content().is_some()
        }));
        assert!(
            !projection
                .files()
                .iter()
                .any(|file| file.source_path().as_str() == "a/b/c")
        );
    }

    #[test]
    fn ten_thousand_flat_files_project_linearly_without_descendant_searches() {
        let mut paths = BTreeMap::new();
        for index in 0..10_000_u128 {
            let counter = u64::try_from(index + 1).expect("counter");
            let clock = VersionVector::new([(actor(0), counter)]).expect("clock");
            let value = RegisterEntry::from_full_clock(
                OpId::new(actor(0), counter).expect("dot"),
                clock,
                EntryValue::File(content((index % 199) as u8 + 1)),
            )
            .expect("entry");
            paths.insert(path(&format!("flat-{index:05}")), register(&[value]));
        }
        let result = project(
            &paths,
            &VersionVector::new([(actor(0), 10_000)]).expect("frontier"),
            ProjectionLimits {
                maximum_paths: 10_000,
                maximum_active_versions: 10_000,
                maximum_output_entries: 10_000,
                maximum_aggregate_path_bytes: 1 << 20,
            },
        )
        .expect("linear flat projection");
        assert_eq!(result.files().len(), 10_000);
        assert!(result.pending().is_empty());
    }

    #[test]
    fn three_peer_merge_and_input_order_have_one_plan() {
        let entries = vec![
            entry(0, EntryValue::File(content(1))),
            entry(1, EntryValue::Directory),
            entry(2, EntryValue::File(content(3))),
        ];
        let one = register(&[entries[0].clone()])
            .merge(&register(&[entries[1].clone()]))
            .expect("merge")
            .merge(&register(&[entries[2].clone()]))
            .expect("merge");
        let two = register(&[entries[2].clone()])
            .merge(&register(&[entries[0].clone()]))
            .expect("merge")
            .merge(&register(&[entries[1].clone()]))
            .expect("merge");
        let front = frontier(&entries);
        let first = project(&BTreeMap::from([(path("x"), one)]), &front, limits()).expect("first");
        let second =
            project(&BTreeMap::from([(path("x"), two)]), &front, limits()).expect("second");
        assert_eq!(first, second);
        assert_no_file_ancestor(&first);
    }

    proptest! {
        /// These are honest independent publications from public register APIs;
        /// randomized delivery here is not a substitute for protocol admission.
        #[test]
        fn every_active_file_is_placed_or_pending_under_three_peer_delivery(
            first in 1_u8..200,
            second in 1_u8..200,
            third in 1_u8..200,
        ) {
            let entries = vec![
                entry(0, EntryValue::File(content(first))),
                entry(1, EntryValue::File(content(second))),
                entry(2, EntryValue::File(content(third))),
            ];
            let left = register(&[entries[0].clone()])
                .merge(&register(&[entries[1].clone()])).expect("merge")
                .merge(&register(&[entries[2].clone()])).expect("merge");
            let right = register(&[entries[2].clone()])
                .merge(&register(&[entries[0].clone()])).expect("merge")
                .merge(&register(&[entries[1].clone()])).expect("merge");
            let front = frontier(&entries);
            let left = project(&BTreeMap::from([(path("same.bin"), left)]), &front, limits()).expect("left");
            let right = project(&BTreeMap::from([(path("same.bin"), right)]), &front, limits()).expect("right");
            prop_assert_eq!(&left, &right);
            prop_assert_eq!(left.files().len() + left.pending().iter().filter(|pending| pending.content().is_some()).count(), 3);
            assert_no_file_ancestor(&left);
        }
    }
}
