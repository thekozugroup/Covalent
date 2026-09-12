use time::OffsetDateTime;
use uuid::Uuid;

use super::controller::validate_initial_scan_health;
use super::{EngineSessionError, FolderHealth, FolderLifecycle};

fn healthy(folder: Uuid) -> FolderHealth {
    FolderHealth {
        folder,
        lifecycle: FolderLifecycle::Idle,
        state_changed: OffsetDateTime::UNIX_EPOCH,
        remaining_files: 0,
        remaining_bytes: 0,
        scan_pull_error_count: 0,
        reported_error_rows: 0,
        status_error: false,
        watch_error: false,
    }
}

#[test]
fn final_scan_health_requires_every_exact_folder_without_errors() {
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    validate_initial_scan_health(&[first, second], &[healthy(first), healthy(second)]).unwrap();

    assert_eq!(
        validate_initial_scan_health(&[], &[]).unwrap_err(),
        EngineSessionError::EngineUnavailable
    );

    for observed in [
        Vec::new(),
        vec![healthy(first)],
        vec![healthy(second), healthy(first)],
    ] {
        assert_eq!(
            validate_initial_scan_health(&[first, second], &observed).unwrap_err(),
            EngineSessionError::EngineUnavailable
        );
    }

    let mut explicit_error = healthy(first);
    explicit_error.scan_pull_error_count = 1;
    assert_eq!(
        validate_initial_scan_health(&[first], &[explicit_error]).unwrap_err(),
        EngineSessionError::EngineUnavailable
    );

    let mut error_state = healthy(first);
    error_state.lifecycle = FolderLifecycle::Error;
    assert_eq!(
        validate_initial_scan_health(&[first], &[error_state]).unwrap_err(),
        EngineSessionError::EngineUnavailable
    );
}
