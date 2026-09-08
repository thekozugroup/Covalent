//! Folder-sync identifiers that are intentionally distinct from backup identities.

use std::fmt;

use covalent_protocol::DeviceId;
use uuid::Uuid;

/// Stable identifier for one explicitly shared folder.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FolderId(Uuid);

impl FolderId {
    /// Constructs an identifier from a UUID allocated by a higher-level workflow.
    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    /// Returns the UUID bytes used by the canonical operation codec.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 16] {
        *self.0.as_bytes()
    }
}

impl fmt::Display for FolderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Per-install author identifier for one folder participant.
///
/// Recovery of an owner's backup identity must not reuse this identifier or
/// its folder-global operation counter. Generation and persistence belong to
/// the future membership/runtime layer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WriterId(Uuid);

impl WriterId {
    /// Constructs an identifier from a UUID allocated by a higher-level workflow.
    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    /// Returns the UUID bytes used by the canonical operation codec.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 16] {
        *self.0.as_bytes()
    }

    /// Bridges this opaque actor into the current causal-math representation.
    ///
    /// This does not turn a writer into a backup device identity or grant it
    /// membership.
    pub(crate) const fn into_vector_actor(self) -> DeviceId {
        DeviceId::from_uuid(self.0)
    }
}

impl fmt::Display for WriterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
