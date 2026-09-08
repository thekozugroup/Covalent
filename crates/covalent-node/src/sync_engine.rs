//! Private-process adapter for the pinned maintained folder-sync engine.
//!
//! This module is not enabled by node startup or exposed as a local API yet.
//! Engine control stays on an authenticated, owner-only Unix socket; native
//! clients must use Covalent's own authorization and folder-sharing workflow.

mod client;
pub mod config;
mod controller;
mod identity;
mod installation;
mod supervisor;

pub use client::{EngineApiClient, EngineApiError, EngineEndpoint};
pub use identity::{EngineIdentity, EngineIdentityError};

pub use supervisor::{
    EngineSupervisorError, OwnedEngineWorker, StopOutcome, VerifiedEngineExecutable,
};

pub use controller::{EngineSessionError, EngineSessionSettings, ManagedEngineSession};
pub use installation::{EngineInstallation, EngineInstallationError};
