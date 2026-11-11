//! Why this device's own attempt at a link failed, in terms a person can act on.
//!
//! The transfer worker's errors are already reduced to a fixed, path-free set
//! of causes for the log. This module keeps the most useful one per folder so
//! the local API can say what went wrong and what to do about it.
//!
//! It is deliberately in memory and local. The persisted run state and the
//! peer protocol carry only "failed", and that stays true: nothing here is
//! written to disk or sent to another device, so a restart forgets it and a
//! device only ever knows the reason for its own attempts.

use std::cell::RefCell;
use std::collections::BTreeSet;

use serde::Serialize;
use uuid::Uuid;

use super::EngineSessionError;

/// Stable reason for a failed attempt. New values may be added.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RunFailureCode {
    /// The destination ran out of storage space.
    NoSpace,
    /// A file or folder could not be read or written for lack of permission.
    PermissionDenied,
    /// The destination folder is read-only.
    ReadOnly,
    /// A copied file did not match its source.
    IntegrityMismatch,
    /// A file or folder expected by the copy was missing.
    NotFound,
    /// The connection between the devices failed.
    Network,
    /// The other device or the transfer took too long.
    Timeout,
    /// Source files changed while they were being copied.
    SourceChanged,
    /// This device could not open the link's folder.
    FolderUnavailable,
    /// An earlier interrupted copy has to be reviewed first.
    RecoveryRequired,
    /// The transfer engine could not start or stopped.
    WorkerUnavailable,
    /// The attempt failed and no specific cause was recognized.
    Unknown,
}

impl RunFailureCode {
    /// One plain-language sentence or two: what happened and what to do.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NoSpace => {
                "This device ran out of storage space for the copy. \
                 Free up space, then run the link again."
            }
            Self::PermissionDenied => {
                "Covalent was not allowed to read or write some of the files. \
                 Check the permissions of the source folder and the destination folder, \
                 then run the link again."
            }
            Self::ReadOnly => {
                "The destination folder cannot be written to. \
                 Make it writable or choose a different destination folder, \
                 then run the link again."
            }
            Self::IntegrityMismatch => {
                "Some files did not match the source after copying. \
                 Run the link again; if it keeps happening, check this device's storage."
            }
            Self::NotFound => {
                "A file or folder was missing during the copy. \
                 Check that the source folder and the destination folder still exist, \
                 then run the link again."
            }
            Self::Network => {
                "The connection to the source was lost during the copy. \
                 Check that both devices are online and can reach each other, \
                 then run the link again."
            }
            Self::Timeout => {
                "The source took too long to respond. \
                 Check the connection between the devices, then run the link again."
            }
            Self::SourceChanged => {
                "Files at the source changed while they were being copied. \
                 Nothing needs fixing; the next run copies the new versions."
            }
            Self::FolderUnavailable => {
                "Covalent could not open this link's folder on this device. \
                 Check that the folder exists and its drive is connected, \
                 or choose the folder again."
            }
            Self::RecoveryRequired => {
                "An earlier copy on this device was interrupted and has to be \
                 reviewed before this link can continue."
            }
            Self::WorkerUnavailable => {
                "Covalent's transfer engine could not start or stopped unexpectedly \
                 on this device. Restart Covalent, then run the link again."
            }
            Self::Unknown => {
                "The copy did not finish and Covalent could not tell why. \
                 Run the link again; this device's log has a COVALENT_SYNC_DIAGNOSTIC \
                 line with more detail."
            }
        }
    }

    /// The best that can be said from the redacted session error alone.
    pub(super) const fn from_session_error(error: EngineSessionError) -> Self {
        match error {
            EngineSessionError::FolderUnavailable => Self::FolderUnavailable,
            EngineSessionError::PendingCopyRecoveryRequired => Self::RecoveryRequired,
            EngineSessionError::RuntimeUnavailable
            | EngineSessionError::LaunchFailed
            | EngineSessionError::StartupTimeout
            | EngineSessionError::StartupMismatch
            | EngineSessionError::EngineUnavailable => Self::WorkerUnavailable,
            EngineSessionError::InvalidConfiguration | EngineSessionError::TransferFailed => {
                Self::Unknown
            }
        }
    }
}

/// Which side of the link this device was for the failed attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RunFailureRole {
    /// This device holds the source folder and could not prepare the run.
    Source,
    /// This device receives the files and its copy failed.
    Destination,
}

/// This device's most recent failed attempt at one link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalRunFailure {
    pub folder_id: Uuid,
    /// Run generation the attempt belonged to; compare with the link's run summary.
    pub generation: u64,
    pub role: RunFailureRole,
    pub code: RunFailureCode,
    pub ended_at_unix_ms: u64,
}

#[derive(Default)]
struct Notes {
    causes: BTreeSet<&'static str>,
    source_changed: bool,
    timed_out: bool,
}

impl Notes {
    fn code(&self, error: EngineSessionError) -> RunFailureCode {
        // When the worker reported several causes, the one a person can act on
        // most directly wins. `causes` holds the log classifier's fixed names.
        for (cause, code) in [
            ("noSpace", RunFailureCode::NoSpace),
            ("http507", RunFailureCode::NoSpace),
            ("readOnly", RunFailureCode::ReadOnly),
            ("permission", RunFailureCode::PermissionDenied),
            ("http401", RunFailureCode::PermissionDenied),
            ("http403", RunFailureCode::PermissionDenied),
            ("integrityMismatch", RunFailureCode::IntegrityMismatch),
            ("notFound", RunFailureCode::NotFound),
            ("http404", RunFailureCode::NotFound),
            ("network", RunFailureCode::Network),
            ("timeout", RunFailureCode::Timeout),
            ("http408", RunFailureCode::Timeout),
            ("http504", RunFailureCode::Timeout),
        ] {
            if self.causes.contains(cause) {
                return code;
            }
        }
        if self.source_changed {
            RunFailureCode::SourceChanged
        } else if self.timed_out {
            RunFailureCode::Timeout
        } else {
            RunFailureCode::from_session_error(error)
        }
    }
}

tokio::task_local! {
    static NOTES: RefCell<Notes>;
}

/// Receives every transfer diagnostic. Outside an [`observed`] attempt this
/// does nothing, and it can never fail or delay the transfer that called it.
pub(super) fn note(operation: &str, category: &str, causes: &BTreeSet<&'static str>) {
    let _ = NOTES.try_with(|notes| {
        let Ok(mut notes) = notes.try_borrow_mut() else {
            return;
        };
        notes.causes.extend(causes.iter().copied());
        match (operation, category) {
            ("source-index" | "source-after-copy", "TransferFailed") => notes.source_changed = true,
            (_, "TimedOut") => notes.timed_out = true,
            _ => {}
        }
    });
}

/// Runs one attempt unchanged and, only if it fails, also returns the most
/// actionable cause the worker reported while it ran.
pub(super) async fn observed<T>(
    attempt: impl Future<Output = Result<T, EngineSessionError>>,
) -> Result<T, (EngineSessionError, RunFailureCode)> {
    NOTES
        .scope(RefCell::new(Notes::default()), async {
            attempt.await.map_err(|error| {
                let code = NOTES
                    .try_with(|notes| {
                        notes
                            .try_borrow()
                            .map_or(RunFailureCode::from_session_error(error), |notes| {
                                notes.code(error)
                            })
                    })
                    .unwrap_or(RunFailureCode::from_session_error(error));
                (error, code)
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn causes(names: &[&'static str]) -> BTreeSet<&'static str> {
        names.iter().copied().collect()
    }

    async fn failed_after(
        notes: &[(&str, &str, &[&'static str])],
        error: EngineSessionError,
    ) -> RunFailureCode {
        observed(async {
            for (operation, category, names) in notes {
                note(operation, category, &causes(names));
                tokio::task::yield_now().await;
            }
            Err::<(), _>(error)
        })
        .await
        .unwrap_err()
        .1
    }

    #[tokio::test]
    async fn the_most_actionable_reported_cause_is_chosen() {
        use EngineSessionError::TransferFailed;
        for (reported, expected) in [
            (&["noSpace"][..], RunFailureCode::NoSpace),
            (&["http507"], RunFailureCode::NoSpace),
            (&["permission"], RunFailureCode::PermissionDenied),
            (&["http403"], RunFailureCode::PermissionDenied),
            (&["readOnly"], RunFailureCode::ReadOnly),
            (&["integrityMismatch"], RunFailureCode::IntegrityMismatch),
            (&["notFound"], RunFailureCode::NotFound),
            (&["network"], RunFailureCode::Network),
            (&["timeout"], RunFailureCode::Timeout),
            // A full disk also breaks the connection; the disk is what to fix.
            (&["network", "noSpace", "timeout"], RunFailureCode::NoSpace),
            (&["network", "permission"], RunFailureCode::PermissionDenied),
            (&["timeout", "network"], RunFailureCode::Network),
            // Causes with no advice of their own fall back to the error.
            (
                &["conflict", "rateLimited", "http500"],
                RunFailureCode::Unknown,
            ),
            (&[], RunFailureCode::Unknown),
        ] {
            assert_eq!(
                failed_after(&[("copy", "NonzeroExit", reported)], TransferFailed).await,
                expected,
                "{reported:?}"
            );
        }
        // Causes from every command of one attempt are considered together.
        assert_eq!(
            failed_after(
                &[
                    ("lsjson", "NonzeroExit", &["network"]),
                    ("copy", "NonzeroExit", &["noSpace"]),
                    ("destination", "TransferFailed", &[]),
                ],
                TransferFailed
            )
            .await,
            RunFailureCode::NoSpace
        );
    }

    #[tokio::test]
    async fn attempts_without_a_worker_message_are_still_explained() {
        use EngineSessionError::{
            EngineUnavailable, FolderUnavailable, LaunchFailed, PendingCopyRecoveryRequired,
            TransferFailed,
        };
        assert_eq!(
            failed_after(&[("source-index", "TransferFailed", &[])], TransferFailed).await,
            RunFailureCode::SourceChanged
        );
        assert_eq!(
            failed_after(
                &[("source-after-copy", "TransferFailed", &[])],
                TransferFailed
            )
            .await,
            RunFailureCode::SourceChanged
        );
        assert_eq!(
            failed_after(&[("copy", "TimedOut", &[])], EngineUnavailable).await,
            RunFailureCode::Timeout
        );
        assert_eq!(
            failed_after(&[("copy", "LaunchFailed", &[])], LaunchFailed).await,
            RunFailureCode::WorkerUnavailable
        );
        assert_eq!(
            failed_after(&[], FolderUnavailable).await,
            RunFailureCode::FolderUnavailable
        );
        assert_eq!(
            failed_after(&[], PendingCopyRecoveryRequired).await,
            RunFailureCode::RecoveryRequired
        );
        assert_eq!(
            failed_after(&[], TransferFailed).await,
            RunFailureCode::Unknown
        );
    }

    #[tokio::test]
    async fn a_successful_attempt_and_an_unobserved_note_change_nothing() {
        // Diagnostics outside an observed attempt are ignored, not an error.
        note("copy", "NonzeroExit", &causes(&["noSpace"]));
        assert_eq!(
            observed(async {
                note("lsjson", "NonzeroExit", &causes(&["network"]));
                Ok::<_, EngineSessionError>(7)
            })
            .await,
            Ok(7)
        );
        // Each attempt starts clean, and attempts running side by side stay apart.
        let (first, second) = tokio::join!(
            failed_after(
                &[("copy", "NonzeroExit", &["noSpace"])],
                EngineSessionError::TransferFailed
            ),
            failed_after(
                &[("copy", "NonzeroExit", &["permission"])],
                EngineSessionError::TransferFailed
            ),
        );
        assert_eq!(first, RunFailureCode::NoSpace);
        assert_eq!(second, RunFailureCode::PermissionDenied);
        assert_eq!(
            failed_after(&[], EngineSessionError::TransferFailed).await,
            RunFailureCode::Unknown
        );
    }

    #[test]
    fn every_code_has_a_documented_spelling_and_a_complete_sentence() {
        for (code, spelling) in [
            (RunFailureCode::NoSpace, "noSpace"),
            (RunFailureCode::PermissionDenied, "permissionDenied"),
            (RunFailureCode::ReadOnly, "readOnly"),
            (RunFailureCode::IntegrityMismatch, "integrityMismatch"),
            (RunFailureCode::NotFound, "notFound"),
            (RunFailureCode::Network, "network"),
            (RunFailureCode::Timeout, "timeout"),
            (RunFailureCode::SourceChanged, "sourceChanged"),
            (RunFailureCode::FolderUnavailable, "folderUnavailable"),
            (RunFailureCode::RecoveryRequired, "recoveryRequired"),
            (RunFailureCode::WorkerUnavailable, "workerUnavailable"),
            (RunFailureCode::Unknown, "unknown"),
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), spelling);
            let message = code.message();
            assert!(message.ends_with('.'), "{spelling}");
            assert!(!message.contains("  "), "{spelling}: {message:?}");
            // Nothing a person reads may name a path, a peer or an internal type.
            assert!(!message.contains('/') && !message.contains("rclone"));
            // The glossary's words: a "link" is what is copied, a "device" runs it.
            let lower = message.to_ascii_lowercase();
            for retired in [
                "share",
                "node",
                "peer",
                "backup server",
                "destination device",
            ] {
                assert!(!lower.contains(retired), "{spelling}: {message:?}");
            }
        }
        assert_eq!(
            serde_json::to_value(RunFailureRole::Source).unwrap(),
            "source"
        );
        assert_eq!(
            serde_json::to_value(RunFailureRole::Destination).unwrap(),
            "destination"
        );
    }
}
