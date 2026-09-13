//! Private-state storage used by the maintained folder-sync runtime.

/// Descriptor-anchored private local state capabilities for folder sync.
#[cfg(unix)]
pub mod state_dir;
