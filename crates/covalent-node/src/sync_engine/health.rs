//! Bounded, per-folder engine health observations.
//!
//! This module intentionally does not infer a successful initial scan from a
//! clean system endpoint. It reads the pinned engine's per-folder status and
//! first page of scan/pull errors. Error strings and paths stay inside the
//! private client boundary and are represented only by fixed booleans/counts.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde::de::{Error as _, IgnoredAny, SeqAccess, Visitor};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::{EngineApiClient, EngineApiError, EngineEndpoint};

const MAX_FOLDERS: usize = 128;
const MAX_ERROR_ROWS: u16 = 128;
const MAX_CONCURRENT_EXCHANGES: usize = 4;
const HEALTH_BUDGET: Duration = Duration::from_secs(10);

/// A redacted folder state defined by the pinned Syncthing v2.1.3 source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderLifecycle {
    Starting,
    Idle,
    Scanning,
    ScanWaiting,
    SyncWaiting,
    SyncPreparing,
    Syncing,
    Cleaning,
    CleanWaiting,
    Error,
}

impl FolderLifecycle {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "starting" => Self::Starting,
            "idle" => Self::Idle,
            "scanning" => Self::Scanning,
            "scan-waiting" => Self::ScanWaiting,
            "sync-waiting" => Self::SyncWaiting,
            "sync-preparing" => Self::SyncPreparing,
            "syncing" => Self::Syncing,
            "cleaning" => Self::Cleaning,
            "clean-waiting" => Self::CleanWaiting,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

/// A bounded observation of one configured folder.
///
/// `reported_error_rows` is the number in the first 128-row page only. It is
/// never a total count and zero means this page was observed empty, not that a
/// full error history or initial scan is complete. `status_error` and
/// `watch_error` similarly retain only whether a nonempty engine field was
/// observed; their messages and paths are deliberately discarded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderHealth {
    pub folder: Uuid,
    pub lifecycle: FolderLifecycle,
    /// Exact RFC 3339 `stateChanged` instant supplied by the engine.
    pub state_changed: OffsetDateTime,
    pub remaining_files: u64,
    pub remaining_bytes: u64,
    /// Engine's current `errors` count, observed in the status response.
    pub scan_pull_error_count: u64,
    /// Rows observed in the first fixed 128-row errors page only.
    pub reported_error_rows: u16,
    pub status_error: bool,
    pub watch_error: bool,
}

impl FolderHealth {
    /// Whether the bounded observation contains an explicit engine error.
    pub const fn has_reported_errors(&self) -> bool {
        self.scan_pull_error_count != 0
            || self.reported_error_rows != 0
            || self.status_error
            || self.watch_error
    }
}

/// Fixed redacted aggregation failures. No private endpoint body is exposed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderHealthError {
    InvalidInput,
    Unavailable,
    TimedOut,
    InvalidResponse,
}

impl fmt::Display for FolderHealthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInput => "invalid folder health request",
            Self::Unavailable => "folder sync engine health is unavailable",
            Self::TimedOut => "folder sync engine health timed out",
            Self::InvalidResponse => "folder sync engine health response is invalid",
        })
    }
}

impl std::error::Error for FolderHealthError {}

/// Collect bounded health observations for controller-approved folder IDs.
///
/// Calls run in batches of at most four; within a folder, status is read before
/// the bounded errors page, keeping at most four HTTP exchanges active. The
/// ten-second budget includes all batches. This function does not establish a
/// global sync/scan completion barrier.
pub async fn collect_folder_health(
    client: &EngineApiClient,
    folders: &[Uuid],
) -> Result<Vec<FolderHealth>, FolderHealthError> {
    if folders.len() > MAX_FOLDERS || folders.iter().collect::<BTreeSet<_>>().len() != folders.len()
    {
        return Err(FolderHealthError::InvalidInput);
    }

    tokio::time::timeout(HEALTH_BUDGET, collect_batched(client, folders))
        .await
        .map_err(|_| FolderHealthError::TimedOut)?
}

async fn collect_batched(
    client: &EngineApiClient,
    folders: &[Uuid],
) -> Result<Vec<FolderHealth>, FolderHealthError> {
    let mut result = Vec::with_capacity(folders.len());
    for batch in folders.chunks(MAX_CONCURRENT_EXCHANGES) {
        match batch {
            [] => unreachable!("chunks never produces an empty batch"),
            [one] => result.push(collect_one(client, *one).await?),
            [one, two] => {
                let (one, two) = tokio::join!(collect_one(client, *one), collect_one(client, *two));
                result.push(one?);
                result.push(two?);
            }
            [one, two, three] => {
                let (one, two, three) = tokio::join!(
                    collect_one(client, *one),
                    collect_one(client, *two),
                    collect_one(client, *three),
                );
                result.push(one?);
                result.push(two?);
                result.push(three?);
            }
            [one, two, three, four] => {
                let (one, two, three, four) = tokio::join!(
                    collect_one(client, *one),
                    collect_one(client, *two),
                    collect_one(client, *three),
                    collect_one(client, *four),
                );
                result.push(one?);
                result.push(two?);
                result.push(three?);
                result.push(four?);
            }
            _ => unreachable!("batch size is bounded by MAX_CONCURRENT_EXCHANGES"),
        }
    }
    Ok(result)
}

async fn collect_one(
    client: &EngineApiClient,
    folder: Uuid,
) -> Result<FolderHealth, FolderHealthError> {
    let status: FolderStatusResponse = client
        .json(EngineEndpoint::FolderStatus(folder))
        .await
        .map_err(map_client_error)?;
    // Reject a malformed status before opening the errors endpoint, so a
    // hostile response cannot consume another request budget.
    let lifecycle =
        FolderLifecycle::parse(&status.state).ok_or(FolderHealthError::InvalidResponse)?;
    let state_changed = OffsetDateTime::parse(&status.state_changed, &Rfc3339)
        .map_err(|_| FolderHealthError::InvalidResponse)?;
    let remaining_files =
        u64::try_from(status.need_files).map_err(|_| FolderHealthError::InvalidResponse)?;
    let remaining_bytes =
        u64::try_from(status.need_bytes).map_err(|_| FolderHealthError::InvalidResponse)?;
    let scan_pull_error_count =
        u64::try_from(status.errors).map_err(|_| FolderHealthError::InvalidResponse)?;

    let errors: FolderErrorsResponse = client
        .json(EngineEndpoint::FolderErrors(folder))
        .await
        .map_err(map_client_error)?;
    if errors.folder != folder.to_string() || errors.page != 1 || errors.perpage != MAX_ERROR_ROWS {
        return Err(FolderHealthError::InvalidResponse);
    }

    Ok(FolderHealth {
        folder,
        lifecycle,
        state_changed,
        remaining_files,
        remaining_bytes,
        scan_pull_error_count,
        reported_error_rows: errors.errors.0,
        status_error: status.error.0,
        watch_error: status.watch_error.0,
    })
}

fn map_client_error(error: EngineApiError) -> FolderHealthError {
    match error {
        EngineApiError::Timeout => FolderHealthError::TimedOut,
        EngineApiError::BodyTooLarge | EngineApiError::InvalidResponse => {
            FolderHealthError::InvalidResponse
        }
        EngineApiError::InvalidConfiguration
        | EngineApiError::UnsafeEndpoint
        | EngineApiError::Unavailable
        | EngineApiError::HttpStatus(_) => FolderHealthError::Unavailable,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FolderStatusResponse {
    state: String,
    state_changed: String,
    need_files: i64,
    need_bytes: i64,
    errors: i64,
    #[serde(default)]
    error: NonemptyText,
    #[serde(default)]
    watch_error: NonemptyText,
}

#[derive(Deserialize)]
struct FolderErrorsResponse {
    folder: String,
    errors: BoundedErrorRows,
    page: u16,
    perpage: u16,
}

/// Consumes error objects without retaining their error strings or file paths.
struct BoundedErrorRows(u16);

impl<'de> Deserialize<'de> for BoundedErrorRows {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RowsVisitor;
        impl<'de> Visitor<'de> for RowsVisitor {
            type Value = BoundedErrorRows;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("at most 128 error rows")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut count = 0_u16;
                while sequence.next_element::<IgnoredAny>()?.is_some() {
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| A::Error::custom("too many error rows"))?;
                    if count > MAX_ERROR_ROWS {
                        return Err(A::Error::custom("too many error rows"));
                    }
                }
                Ok(BoundedErrorRows(count))
            }
        }

        // Pinned v2.1.3 `getFolderErrors` assigns a nil slice when its first
        // page is past (or at) the end. Go encodes that observed empty page as
        // JSON null. Null is therefore the one non-array value accepted here.
        struct OptionalRowsVisitor;
        impl<'de> Visitor<'de> for OptionalRowsVisitor {
            type Value = BoundedErrorRows;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("null or at most 128 error rows")
            }

            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(BoundedErrorRows(0))
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(BoundedErrorRows(0))
            }

            fn visit_some<D: serde::Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                deserializer.deserialize_seq(RowsVisitor)
            }
        }
        deserializer.deserialize_option(OptionalRowsVisitor)
    }
}

/// Accepts an optional string while retaining only whether it is nonempty.
#[derive(Default)]
struct NonemptyText(bool);

impl<'de> Deserialize<'de> for NonemptyText {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor;
        impl<'de> Visitor<'de> for TextVisitor {
            type Value = NonemptyText;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(NonemptyText(!value.is_empty()))
            }

            fn visit_borrowed_str<E: serde::de::Error>(
                self,
                value: &'de str,
            ) -> Result<Self::Value, E> {
                self.visit_str(value)
            }

            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(NonemptyText(!value.is_empty()))
            }
        }
        deserializer.deserialize_string(TextVisitor)
    }
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod tests;
