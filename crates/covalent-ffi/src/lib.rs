//! Narrow service facade intended for generated Swift and Kotlin bindings.

use std::path::Path;

use covalent_core::AuthorizedRoot;
use covalent_protocol::{PROTOCOL_VERSION, RelativePath};
use serde::{Deserialize, Serialize};

/// Stateless foundation service. Stateful job methods are added behind this boundary.
#[derive(Clone, Debug, Default)]
pub struct CovalentService;

impl CovalentService {
    /// Creates a service facade.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Returns the service contract version.
    #[must_use]
    pub const fn protocol_version(&self) -> u16 {
        PROTOCOL_VERSION
    }

    /// Validates a restore destination using the shared engine rules.
    pub fn validate_restore_destination(
        &self,
        authorized_root: impl AsRef<Path>,
        relative_path: &str,
    ) -> Result<RestoreDestination, ServiceError> {
        let root = AuthorizedRoot::open(authorized_root)
            .map_err(|error| ServiceError::from_engine(&error))?;
        let relative = RelativePath::new(relative_path)
            .map_err(|error| ServiceError::new("invalid_relative_path", error.to_string()))?;
        let destination = root
            .resolve(&relative)
            .map_err(|error| ServiceError::from_engine(&error))?;
        Ok(RestoreDestination {
            relative_path: relative.to_string(),
            resolved_path: destination.to_string_lossy().into_owned(),
        })
    }
}

/// Binding-safe validated restore destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreDestination {
    /// Canonical relative protocol path.
    pub relative_path: String,
    /// Local display path beneath the authorized root.
    pub resolved_path: String,
}

/// Stable binding-safe service error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceError {
    /// Stable machine-readable code.
    pub code: String,
    /// Safe local message.
    pub message: String,
}

impl ServiceError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn from_engine(error: &covalent_core::CoreError) -> Self {
        let code = match error {
            covalent_core::CoreError::SymlinkTraversal(_) => "symlink_traversal",
            covalent_core::CoreError::InvalidAuthorizedRoot(_) => "invalid_authorized_root",
            covalent_core::CoreError::NonDirectoryAncestor(_) => "non_directory_ancestor",
            _ => "restore_validation_failed",
        };
        Self::new(code, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn facade_uses_shared_restore_validation() {
        let root = tempdir().expect("temporary root");
        let service = CovalentService::new();
        assert!(
            service
                .validate_restore_destination(root.path(), "nested/file.txt")
                .is_ok()
        );
        assert_eq!(
            service
                .validate_restore_destination(root.path(), "../escape")
                .expect_err("traversal must fail")
                .code,
            "invalid_relative_path"
        );
    }
}
